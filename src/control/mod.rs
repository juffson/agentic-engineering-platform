mod deploy_keys;
mod evidence;
mod http;
mod state;
mod worker;
mod worker_keys;

use crate::git::Repo;
use aep::{
    delivery::{self, Target},
    studio::{Artifact, artifact_file},
    workspace::{Workspace, commit, git, ident, status, validate_task_target},
};
use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand};
use http::Client;
use sha2::{Digest, Sha256};
use state::{Attempt, Operation, Reply, Stage, Task, sha};
use std::{
    ffi::OsString,
    fs::OpenOptions,
    io::Read,
    net::SocketAddr,
    path::{Path, PathBuf},
};

#[derive(Args)]
pub struct Control {
    /// Use HTTPS for remote control planes. Loopback HTTP is for local trials.
    #[arg(long, default_value = "http://127.0.0.1:8787")]
    url: String,
    /// Require an explicitly trusted CA bundle for this HTTPS control endpoint.
    #[arg(long, value_name = "PEM")]
    ca_cert: Option<PathBuf>,
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Serve one project behind an HTTPS reverse proxy.
    Serve {
        #[arg(long, default_value = "127.0.0.1:8787")]
        listen: SocketAddr,
        #[arg(long)]
        state: PathBuf,
        #[arg(long)]
        device: String,
        /// Read additional Worker IDs and unique bearer tokens from a private JSON file.
        #[arg(long)]
        workers: Option<PathBuf>,
        #[arg(long, default_value_t = 60)]
        lease_seconds: u64,
    },
    /// Schedule a task whose Spec already exists on the pinned Git dispatch branch.
    Submit {
        #[arg(long)]
        ticket: String,
        #[arg(long)]
        task: String,
        #[arg(long)]
        spec: String,
        #[arg(long)]
        target: String,
        #[arg(long)]
        baseline: String,
        #[arg(long, default_value_t = 600)]
        timeout: u64,
        #[arg(long, default_value_t = 3)]
        max_attempts: usize,
        /// Invalidate the previous approval and schedule a new pinned Spec for the ticket.
        #[arg(long)]
        revise: bool,
    },
    Status,
    /// Manage repository-scoped Deploy Keys for one HTTPS Worker installation.
    DeployKey {
        #[arg(long)]
        worker: String,
        #[arg(long)]
        installation: String,
        #[arg(long, value_enum)]
        action: deploy_keys::Action,
        #[arg(long)]
        fingerprint: Option<String>,
    },
    /// Pause or resume new Worker leases.
    QueuePause {
        #[arg(long, action = clap::ArgAction::Set)]
        paused: bool,
    },
    /// Swap a queued Run with its currently adjacent neighbor.
    QueueMove {
        #[arg(long)]
        run: String,
        #[arg(long)]
        adjacent: String,
        #[arg(long, action = clap::ArgAction::Set)]
        earlier: bool,
    },
    /// Retrieve immutable, bounded evidence from a terminal remote Run.
    Collect {
        #[arg(long)]
        ticket: String,
        #[arg(long)]
        run: String,
        #[arg(long)]
        output: PathBuf,
    },
    Cancel {
        #[arg(long)]
        ticket: String,
        /// Cancel only this exact current Run of the ticket.
        #[arg(long)]
        run: Option<String>,
        #[arg(long, requires = "run")]
        stage: Option<Stage>,
    },
    Retry {
        #[arg(long)]
        ticket: String,
        /// Find the previous local candidate and rerun checks and review without reimplementation.
        #[arg(long)]
        reverify: bool,
    },
    /// Record the operator's review of the exact local evidence.
    Approve {
        #[arg(long)]
        ticket: String,
        #[arg(long)]
        run: String,
        #[arg(long)]
        spec: String,
        #[arg(long)]
        candidate: String,
    },
    /// Publish an approved candidate and create or recover its GitHub PR.
    Publish {
        #[arg(long)]
        ticket: String,
        #[arg(long)]
        run: String,
        #[arg(long)]
        spec: String,
        #[arg(long)]
        candidate: String,
    },
    /// Start the outbound local execution daemon.
    Worker {
        #[arg(long)]
        codex_model: Option<String>,
        /// Site-provided sandbox launcher that runs the executable and argv it receives.
        #[arg(long)]
        sandbox_launcher: Option<PathBuf>,
        /// Trusted program receiving the checkout, repository, source ref and destination ref.
        #[arg(long)]
        repository_fetcher: Option<PathBuf>,
        /// Worker-owned private directory for managed Deploy Keys.
        #[arg(long, requires_all = ["repository_fetcher", "sandbox_launcher", "journal_dir"])]
        deploy_key_directory: Option<PathBuf>,
        /// Previously provisioned read-only Deploy Key, if this Worker already has one.
        #[arg(long, requires = "deploy_key_directory")]
        legacy_deploy_key: Option<PathBuf>,
        /// Keep device leases and the outbox in a trusted supervisor directory.
        #[arg(long)]
        journal_dir: Option<PathBuf>,
        /// Process at most one Run, including any unacknowledged local result.
        #[arg(long)]
        once: bool,
        /// Send allowlisted terminal evidence to the configured HTTPS control plane.
        #[arg(long)]
        upload_evidence: bool,
    },
}

fn tool_path() -> Result<OsString> {
    std::env::var_os("PATH").context("PATH is unset")
}

fn exact_run(
    client: &Client,
    ticket: &str,
    id: &str,
    spec: &str,
    candidate: &str,
) -> Result<Attempt> {
    let Reply::Snapshot(status) = client.call(Operation::Status)? else {
        bail!("invalid control snapshot");
    };
    let latest = status
        .attempts
        .into_iter()
        .rev()
        .find(|run| run.task.ticket == ticket)
        .context("unknown ticket")?;
    ensure!(
        latest.id == id && latest.task.spec == spec && latest.candidate() == Some(candidate),
        "Run, Spec or candidate changed"
    );
    Ok(latest)
}

fn checked_evidence(ws: &Workspace, run: &Attempt, candidate: &str) -> Result<String> {
    ident(&run.id, 64)?;
    let record = status(&ws.data, &run.id)?;
    ensure!(
        record["status"] == "awaiting_human_review"
            && record["review_status"] == "findings_recorded"
            && record["check_exit_code"] == 0
            && record["spec_sha"] == run.task.spec
            && record["candidate_sha"] == candidate
            && record["task"] == run.task.name,
        "local evidence is missing or doesn't match the passing candidate"
    );
    ensure!(
        commit(&ws.root, &format!("refs/aep/runs/{}", run.id))? == candidate,
        "local candidate reference changed"
    );
    let mut digest = Sha256::new();
    let metadata = serde_json::to_vec(&record)?;
    digest.update((metadata.len() as u64).to_be_bytes());
    digest.update(metadata);
    let mut buffer = [0; 16 * 1024];
    for artifact in [Artifact::Checks, Artifact::Review] {
        let mut file = artifact_file(&ws.data, &run.id, artifact)?
            .context("local approval artifact is missing")?;
        digest.update(file.metadata()?.len().to_be_bytes());
        loop {
            let size = file.read(&mut buffer)?;
            if size == 0 {
                break;
            }
            digest.update(&buffer[..size]);
        }
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn target(
    ws: &Workspace,
    path: &std::ffi::OsStr,
    run: &Attempt,
    candidate: &str,
) -> Result<Target> {
    let repo = delivery::repository(ws, path)?;
    ensure!(
        repo == run.task.repository,
        "origin changed from the approved repository"
    );
    ensure!(
        delivery::remote_sha(ws, path, &format!("refs/heads/aep-task/{}", run.task.name))?
            .as_deref()
            == Some(&run.task.spec),
        "pinned dispatch version changed"
    );
    let target = Target {
        relay: String::new(),
        task: run.task.name.clone(),
        run: run.id.clone(),
        repo,
        base: run.task.target.clone(),
        base_sha: run.task.baseline.clone(),
        merge_base: run.task.baseline.clone(),
        spec: run.task.spec.clone(),
        candidate: candidate.to_owned(),
        head: format!("aep/pr/{}/{}", run.task.name, run.id),
    };
    delivery::check_base(ws, path, &target)?;
    Ok(target)
}

fn publish(
    client: &Client,
    ws: &Workspace,
    run: Attempt,
    candidate: &str,
    report_sha256: String,
) -> Result<Reply> {
    if run.stage == Stage::Published {
        return Ok(Reply::Run(Box::new(run)));
    }
    let path = tool_path()?;
    let target = target(ws, &path, &run, candidate)?;
    let dir = ws.data.join("runs").join(&run.id);
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("delivery.lock"))?;
    file.try_lock()
        .context("this candidate is already being delivered")?;
    let reserved = client.call(Operation::BeginPublication {
        ticket: run.task.ticket.clone(),
        run: run.id.clone(),
        spec: run.task.spec.clone(),
        candidate: candidate.into(),
        report_sha256,
    })?;
    let Reply::Run(reserved) = reserved else {
        bail!("invalid publication reservation");
    };
    if reserved.stage == Stage::Published {
        return Ok(Reply::Run(reserved));
    }
    state::save(&dir.join("control-approval.json"), &reserved.approval)?;
    let pull = delivery::submit(ws, &path, &target, || Ok(()))?;
    ensure!(
        pull.head_sha == candidate
            && pull.base == target.base
            && pull.base_sha == target.base_sha
            && pull.state == "open",
        "GitHub PR differs from the approved candidate or base"
    );
    state::save(&dir.join("control-publication.json"), &pull)?;
    client.call(Operation::Published {
        ticket: run.task.ticket,
        run: run.id,
        candidate: candidate.into(),
        pr: pull.number,
    })
}

pub fn execute(repo: &Path, request: Control) -> Result<()> {
    let Control {
        url,
        ca_cert,
        action,
    } = request;
    let action = match action {
        Action::Serve {
            listen,
            state,
            device,
            lease_seconds,
            workers,
        } => return http::serve(listen, state, device, lease_seconds, workers.as_deref()),
        Action::Worker {
            codex_model,
            sandbox_launcher,
            repository_fetcher,
            deploy_key_directory,
            legacy_deploy_key,
            journal_dir,
            once,
            upload_evidence,
        } => {
            let sandbox = sandbox_launcher
                .map(|path| path.canonicalize())
                .transpose()?;
            let fetcher = repository_fetcher
                .map(|path| path.canonicalize())
                .transpose()?;
            return worker::run(
                &Repo::open(repo)?,
                &url,
                ca_cert.as_deref(),
                journal_dir.as_deref(),
                deploy_key_directory.as_deref().zip(fetcher.as_deref()).map(
                    |(directory, fetcher)| worker::KeyOptions {
                        directory,
                        fetcher,
                        legacy: legacy_deploy_key.as_deref(),
                    },
                ),
                once,
                worker::LaunchOptions {
                    model: codex_model.as_deref(),
                    sandbox: sandbox.as_deref(),
                    fetcher: fetcher.as_deref(),
                    key: None,
                    upload_evidence,
                },
            );
        }
        action => action,
    };
    let client = Client::new(&url, false, ca_cert.as_deref())?;
    let reply = match action {
        Action::Status => client.call(Operation::Status)?,
        Action::DeployKey {
            worker,
            installation,
            action,
            fingerprint,
        } => {
            ensure!(
                url.starts_with("https://"),
                "Deploy Key operations require HTTPS"
            );
            deploy_keys::execute(
                &client,
                &Workspace::open(repo)?,
                &tool_path()?,
                worker,
                installation,
                action,
                fingerprint.as_deref(),
            )?
        }
        Action::Collect {
            ticket,
            run,
            output,
        } => {
            evidence::unpack(&output, &run, &client.download(&ticket, &run)?)?;
            return Ok(());
        }
        Action::QueuePause { paused } => client.call(Operation::QueuePause { paused })?,
        Action::QueueMove {
            run,
            adjacent,
            earlier,
        } => client.call(Operation::QueueMove {
            run,
            adjacent,
            earlier,
        })?,
        Action::Cancel { ticket, run, stage } => client.call(Operation::Cancel {
            ticket,
            expected_run: run,
            expected_stage: stage,
        })?,
        Action::Retry { ticket, reverify } => client.call(Operation::Retry { ticket, reverify })?,
        Action::Submit {
            ticket,
            task,
            spec,
            target,
            baseline,
            timeout,
            max_attempts,
            revise,
        } => {
            let repo = Repo::open(repo)?;
            ensure!(
                sha(&spec) && sha(&baseline),
                "supply full immutable commit SHAs"
            );
            ensure!(
                repo.dispatched(&task, "origin")? == spec,
                "Git dispatch does not match Spec"
            );
            git(&repo.root, &["check-ref-format", "--branch", &target])?;
            let reference = format!("refs/heads/{target}");
            let remote = git(&repo.root, &["ls-remote", "--refs", "origin", &reference])?;
            ensure!(
                remote.trim_end() == format!("{baseline}\t{reference}"),
                "target branch no longer matches the pinned baseline"
            );
            validate_task_target(&repo.root, &baseline, &spec, &task)?;
            repo.task(&spec, &task)?;
            let repository =
                delivery::slug(git(&repo.root, &["config", "--get", "remote.origin.url"])?.trim())?;
            client.call(Operation::Dispatch {
                task: Task {
                    ticket,
                    name: task,
                    repository,
                    spec,
                    target,
                    baseline,
                    timeout,
                    max_attempts,
                },
                revise,
            })?
        }
        Action::Approve {
            ticket,
            run,
            spec,
            candidate,
        } => {
            let current = exact_run(&client, &ticket, &run, &spec, &candidate)?;
            let ws = Workspace::open(repo)?;
            let report_sha256 = checked_evidence(&ws, &current, &candidate)?;
            let path = tool_path()?;
            target(&ws, &path, &current, &candidate)?;
            let user = delivery::github(&ws, &path, &["user"])?;
            let actor = user["login"]
                .as_str()
                .context("GitHub approver identity missing")?
                .into();
            client.call(Operation::Approve {
                ticket,
                run,
                actor,
                spec,
                candidate,
                report_sha256,
            })?
        }
        Action::Publish {
            ticket,
            run,
            spec,
            candidate,
        } => {
            let current = exact_run(&client, &ticket, &run, &spec, &candidate)?;
            let ws = Workspace::open(repo)?;
            let report_sha256 = checked_evidence(&ws, &current, &candidate)?;
            publish(&client, &ws, current, &candidate, report_sha256)?
        }
        Action::Serve { .. } | Action::Worker { .. } => unreachable!(),
    };
    println!("{}", serde_json::to_string_pretty(&reply)?);
    Ok(())
}
