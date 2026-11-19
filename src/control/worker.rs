//! A Studio-independent, outbound worker. The local outbox survives worker and network restarts.
use super::{
    http::{Client, DEVICE_TOKEN, OPERATOR_TOKEN, Rejected, now},
    state::{Event, Lease, Operation, Outcome, Phase, Profile, Progress, Reply, save, sha},
};
use crate::git::Repo;
use aep::{
    cancel::Cancellation,
    workspace::{commit, git, ident, status, validate_task_target},
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::{self, File, OpenOptions, TryLockError},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::Duration,
};

const POLL: Duration = Duration::from_secs(2);

#[derive(Deserialize, Serialize)]
struct Journal {
    lease: Lease,
    launched_at: Option<u64>,
    #[serde(default)]
    stale: bool,
    #[serde(default)]
    timed_out: bool,
    events: Vec<Event>,
    acknowledged: usize,
}

impl Journal {
    fn push(&mut self, progress: Progress, path: &Path) -> Result<()> {
        if self.events.last().is_some_and(|last| {
            last.progress == progress || matches!(last.progress, Progress::Exit { .. })
        }) {
            return Ok(());
        }
        self.events.push(Event {
            sequence: self.events.len() + 1,
            unix_seconds: now(),
            progress,
        });
        save(path, self)
    }

    fn exit_seen(&self) -> bool {
        self.events
            .last()
            .is_some_and(|last| matches!(last.progress, Progress::Exit { .. }))
    }

    fn flush(&mut self, client: &Client, path: &Path) -> Result<()> {
        while let Some(event) = self.events.get(self.acknowledged) {
            let reply = client.call(Operation::Report {
                run: self.lease.run.clone(),
                fence: self.lease.fence.clone(),
                event: event.clone(),
            })?;
            ensure!(
                matches!(reply, Reply::Ack(sequence) if sequence == event.sequence),
                "mismatched event acknowledgement"
            );
            self.acknowledged = event.sequence;
            save(path, self)?;
        }
        Ok(())
    }

    fn renew(&self, client: &Client) -> Result<(bool, bool)> {
        match client.call(Operation::Renew {
            run: self.lease.run.clone(),
            fence: self.lease.fence.clone(),
        })? {
            Reply::Renewed { cancel, stale } => Ok((cancel, stale)),
            _ => bail!("invalid lease response"),
        }
    }
}

fn report(record: &Value, elapsed_timeout: bool) -> Option<Progress> {
    let candidate = record["candidate_sha"]
        .as_str()
        .filter(|value| sha(value))
        .map(str::to_owned);
    let stage = record["status"].as_str()?;
    let outcome = match stage {
        "awaiting_human_review"
            if record["check_exit_code"] == 0
                && record["review_status"] == "findings_recorded"
                && candidate.is_some() =>
        {
            Outcome::Passed
        }
        "cancelled" if elapsed_timeout => Outcome::TimedOut,
        "cancelled" => Outcome::Cancelled,
        "needs_reconciliation" => Outcome::Crashed,
        "failed"
            if record["error"]
                .as_str()
                .is_some_and(|error| error.contains("timed out")) =>
        {
            Outcome::TimedOut
        }
        "failed" | "awaiting_human_review" => Outcome::Failed,
        "running" | "initializing" => {
            let phase = match record["phase"].as_str() {
                Some("implementing") => Phase::Implementing,
                Some("checking") => Phase::Checking,
                Some("reviewing") => Phase::Reviewing,
                _ => Phase::Preparing,
            };
            return Some(Progress::Phase { phase, candidate });
        }
        _ => return None,
    };
    Some(Progress::Exit { outcome, candidate })
}

fn version(program: &str) -> Result<String> {
    let output = Command::new(program)
        .arg("--version")
        .env_remove(OPERATOR_TOKEN)
        .env_remove(DEVICE_TOKEN)
        .output()?;
    ensure!(output.status.success(), "cannot detect {program} version");
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn prepare(repo: &Repo, lease: &Lease) -> Result<Option<String>> {
    let task = &lease.task;
    let origin = git(&repo.root, &["config", "--get", "remote.origin.url"])?;
    ensure!(
        aep::delivery::slug(origin.trim())? == task.repository,
        "managed task belongs to another repository"
    );
    ident(&task.name, 63)?;
    ensure!(
        sha(&task.spec) && sha(&task.baseline),
        "invalid pinned Git versions"
    );
    let reference = format!("refs/heads/aep-task/{}", task.name);
    let pinned = format!("refs/aep/control/{}", lease.run);
    git(
        &repo.root,
        &[
            "fetch",
            "--no-tags",
            "origin",
            &format!("{reference}:{pinned}"),
        ],
    )?;
    ensure!(
        commit(&repo.root, &pinned)? == task.spec,
        "remote Spec changed"
    );
    validate_task_target(&repo.root, &task.baseline, &task.spec, &task.name)?;
    repo.task(&task.spec, &task.name)?;
    if !lease.reverify {
        return Ok(None);
    }
    let prior = lease
        .predecessor
        .as_deref()
        .context("missing recovery Run")?;
    ident(prior, 64)?;
    let candidate = commit(&repo.root, &format!("refs/aep/runs/{prior}"))?;
    let previous = status(&repo.data, prior)?;
    ensure!(
        previous["spec_sha"] == task.spec
            && previous["candidate_sha"]
                .as_str()
                .is_none_or(|sha| sha == candidate),
        "recovery candidate does not match the pinned Run"
    );
    Ok(Some(candidate))
}

#[derive(Clone, Copy)]
struct LaunchOptions<'a> {
    model: Option<&'a str>,
    sandbox: Option<&'a Path>,
}

fn launch(
    repo: &Repo,
    lease: &Lease,
    folder: &Path,
    candidate: Option<&str>,
    options: LaunchOptions<'_>,
) -> Result<Child> {
    let log = File::create(folder.join("worker.log"))?;
    let executable = std::env::current_exe()?;
    let mut command = if let Some(sandbox) = options.sandbox {
        let mut command = Command::new(sandbox);
        command.arg(executable);
        command
    } else {
        Command::new(executable)
    };
    command
        .arg("--repo")
        .arg(&repo.root)
        .args([
            "run",
            "--task",
            &lease.task.name,
            "--base",
            &lease.task.spec,
            "--run-id",
            &lease.run,
            "--reviewer",
            "claude",
            "--shared-memory",
            "--timeout",
            &lease.task.timeout.to_string(),
        ])
        .env_remove(OPERATOR_TOKEN)
        .env_remove(DEVICE_TOKEN)
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_ENTERPRISE_TOKEN")
        .env_remove("GITHUB_ENTERPRISE_TOKEN")
        .env_remove("SSH_AUTH_SOCK")
        .env_remove("GIT_ASKPASS")
        .env_remove("GIT_SSH_COMMAND")
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    if let Some(candidate) = candidate {
        command.args(["--candidate", candidate]);
    } else {
        command.args(["--implementer", "codex"]);
        if let Some(model) = options.model {
            command.args(["--codex-model", model]);
        }
    }
    command.spawn().context("start local AEP executor")
}

fn sync_run(
    client: &Client,
    repo: &Repo,
    folder: &Path,
    journal: &mut Journal,
    child: &mut Option<Child>,
    options: LaunchOptions<'_>,
) -> Result<bool> {
    let state_file = folder.join("events.json");
    let cancellation = Cancellation::new(&repo.data, &journal.lease.run)?;
    let acknowledged = match journal.flush(client, &state_file) {
        Ok(()) => true,
        Err(error) if error.is::<Rejected>() => {
            return Err(error).context("local outbox requires reconciliation");
        }
        Err(error) => {
            eprintln!("Control temporarily unavailable: {error}");
            false
        }
    };
    if acknowledged && journal.exit_seen() {
        return Ok(true);
    }
    let mut cancel_requested = false;
    let mut connected = false;
    if acknowledged && !journal.stale {
        match journal.renew(client) {
            Ok((cancel, stale)) => {
                cancel_requested = cancel;
                connected = !stale;
                if stale {
                    eprintln!("Run {} lost its lease", journal.lease.run);
                    journal.stale = true;
                    save(&state_file, journal)?;
                }
            }
            Err(error) => eprintln!("Control temporarily unavailable: {error}"),
        }
    }
    if let Some(started) = journal.launched_at {
        if !journal.timed_out && now() >= started.saturating_add(journal.lease.task.timeout) {
            journal.timed_out = cancellation.request()?;
            save(&state_file, journal)?;
        }
        if cancel_requested || journal.stale {
            cancellation.request()?;
        }
        let exited = if let Some(handle) = child {
            handle.try_wait()?.is_some()
        } else {
            false
        };
        if exited {
            *child = None;
        }
        let local_record = status(&repo.data, &journal.lease.run).ok();
        let progress = local_record
            .as_ref()
            .and_then(|record| report(record, journal.timed_out));
        match progress {
            Some(progress @ Progress::Phase { .. }) => journal.push(progress, &state_file)?,
            Some(progress) if child.is_none() && executor_idle(repo, &journal.lease.run)? => {
                journal.push(progress, &state_file)?;
            }
            None if exited
                || (now() >= started.saturating_add(8)
                    && child.is_none()
                    && executor_idle(repo, &journal.lease.run)?) =>
            {
                let outcome = if cancel_requested {
                    Outcome::Cancelled
                } else if journal.timed_out {
                    Outcome::TimedOut
                } else {
                    Outcome::Crashed
                };
                journal.push(
                    Progress::Exit {
                        outcome,
                        candidate: None,
                    },
                    &state_file,
                )?;
            }
            _ => (),
        }
    } else if cancel_requested || journal.stale {
        journal.push(
            Progress::Exit {
                outcome: Outcome::Cancelled,
                candidate: None,
            },
            &state_file,
        )?;
    } else if connected {
        match prepare(repo, &journal.lease) {
            Ok(candidate) => {
                match journal.renew(client) {
                    Ok((false, false)) => (),
                    Ok(_) => return Ok(false),
                    Err(error) => {
                        eprintln!("Control unavailable after preflight: {error}");
                        return Ok(false);
                    }
                }
                journal.launched_at = Some(now());
                save(&state_file, journal)?;
                match launch(repo, &journal.lease, folder, candidate.as_deref(), options) {
                    Ok(process) => *child = Some(process),
                    Err(error) => {
                        fs::write(folder.join("worker-error.txt"), format!("{error:#}"))?;
                        journal.push(
                            Progress::Exit {
                                outcome: Outcome::Blocked,
                                candidate: None,
                            },
                            &state_file,
                        )?;
                    }
                }
            }
            Err(error) => {
                fs::write(folder.join("worker-error.txt"), format!("{error:#}"))?;
                journal.push(
                    Progress::Exit {
                        outcome: Outcome::Blocked,
                        candidate: None,
                    },
                    &state_file,
                )?;
            }
        }
    }
    Ok(false)
}

fn executor_idle(repo: &Repo, run: &str) -> Result<bool> {
    let lock = repo.data.join("runs").join(run).join("owner.lock");
    let file = match File::open(lock) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error.into()),
    };
    match file.try_lock_shared() {
        Ok(()) => Ok(true),
        Err(TryLockError::WouldBlock) => Ok(false),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}

pub fn run(
    repo: &Repo,
    endpoint: &str,
    model: Option<&str>,
    sandbox: Option<&Path>,
    once: bool,
) -> Result<()> {
    let options = LaunchOptions { model, sandbox };
    let base = repo.data.join("control-worker");
    fs::create_dir_all(&base)?;
    let owner = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(base.join("worker.lock"))?;
    owner
        .try_lock()
        .context("this repository already has an active control worker")?;
    let marker = base.join("active.json");
    let installation_file = base.join("installation.json");
    let installation = if installation_file.try_exists()? {
        serde_json::from_slice::<String>(&fs::read(&installation_file)?)?
    } else {
        let id = uuid::Uuid::new_v4().simple().to_string();
        save(&installation_file, &id)?;
        id
    };
    let client = Client::new(endpoint, true)?;
    let profile = Profile {
        codex: version("codex").context("install Codex CLI")?,
        claude: version("claude").context("install Claude Code")?,
    };
    let mut child = None;

    loop {
        if !marker.try_exists()? {
            match client.call(Operation::Poll {
                installation: installation.clone(),
                profile: profile.clone(),
            }) {
                Ok(Reply::Polled(Some(lease))) => {
                    ident(&lease.run, 64)?;
                    let folder = base.join(&lease.run);
                    fs::create_dir_all(&folder)?;
                    let state_file = folder.join("events.json");
                    if !state_file.try_exists()? {
                        save(
                            &state_file,
                            &Journal {
                                lease: lease.clone(),
                                launched_at: None,
                                stale: false,
                                timed_out: false,
                                events: Vec::new(),
                                acknowledged: 0,
                            },
                        )?;
                    }
                    save(&marker, &lease.run)?;
                }
                Ok(Reply::Polled(None)) if once => return Ok(()),
                Ok(Reply::Polled(None)) => (),
                Ok(_) => bail!("invalid work poll response"),
                Err(error) if once => return Err(error),
                Err(error) => eprintln!("Control temporarily unavailable: {error}"),
            }
        }
        if marker.try_exists()? {
            let run: String = serde_json::from_slice(&fs::read(&marker)?)?;
            ident(&run, 64)?;
            let folder: PathBuf = base.join(&run);
            let mut journal: Journal =
                serde_json::from_slice(&fs::read(folder.join("events.json"))?)?;
            if sync_run(&client, repo, &folder, &mut journal, &mut child, options)? {
                // The last event remains in the local outbox for audit.
                fs::remove_file(&marker)?;
                if once {
                    return Ok(());
                }
            }
        }
        thread::sleep(POLL);
    }
}
