use crate::{
    Execution,
    agent::{self, Agents},
    git::{Repo, Task, commit, git, ident},
    memory::{Key, Memory},
};
use aep::{
    cancel::{Cancellation, Cancelled},
    studio::{Artifact, artifact_file},
    workflow::{Client, Profile},
};
use anyhow::{Context, Result, bail, ensure};
use chrono::Utc;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, DirBuilder, File, OpenOptions, TryLockError},
    io::{self, Read, Write},
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, Instant},
};

pub enum Base<'a> {
    Remote(&'a str),
    Local(&'a str),
}
pub use aep::workspace::read;
pub fn save(folder: &Path, record: &Value) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(folder)?;
    serde_json::to_writer_pretty(&mut file, record)?;
    writeln!(file)?;
    file.flush()?;
    file.persist(folder.join("evidence.json"))?;
    Ok(())
}
fn directory(repo: &Repo, id: &str) -> Result<PathBuf> {
    ident(id, 64)?;
    Ok(repo.data.join("runs").join(id))
}
pub fn status(repo: &Repo, id: &str) -> Result<Value> {
    aep::workspace::status(&repo.data, id)
}

pub fn report_digest(record: &Value, artifacts: [(u64, impl Read); 2]) -> Result<String> {
    let mut digest = Sha256::new();
    let metadata = serde_json::to_vec(record)?;
    digest.update((metadata.len() as u64).to_be_bytes());
    digest.update(metadata);
    let mut buffer = [0; 16 * 1024];
    for (length, mut artifact) in artifacts {
        digest.update(length.to_be_bytes());
        loop {
            let size = artifact.read(&mut buffer)?;
            if size == 0 {
                break;
            }
            digest.update(&buffer[..size]);
        }
    }
    Ok(format!("{digest:x}", digest = digest.finalize()))
}

pub fn approval_digest(data: &Path, id: &str, record: &Value) -> Result<String> {
    let open = |artifact| -> Result<_> {
        let file =
            artifact_file(data, id, artifact)?.context("local approval artifact is missing")?;
        Ok((file.metadata()?.len(), file))
    };
    report_digest(record, [open(Artifact::Checks)?, open(Artifact::Review)?])
}

fn retry_feedback(repo: &Repo, args: &Execution, base: &str) -> Result<Option<Value>> {
    let Some(id) = &args.feedback_run else {
        return Ok(None);
    };
    let record = status(repo, id)?;
    ensure!(
        record["task"] == args.task
            && record["spec_sha"] == base
            && matches!(
                record["status"].as_str(),
                Some("failed" | "cancelled" | "needs_reconciliation")
            )
            && record["candidate_sha"].as_str() == args.candidate.as_deref(),
        "feedback must come from a failed attempt of this exact Spec and candidate"
    );
    let checks = record["acceptance"].as_array().map(|checks| {
        checks
            .iter()
            .map(|c| json!({"id":c["id"], "status":c["status"], "exit_code":c["exit_code"]}))
            .collect::<Vec<_>>()
    });
    Ok(Some(
        json!({"run":id, "checks":checks, "blocking_findings":record["blocking_findings"]}),
    ))
}
fn replay(repo: &Repo, args: &Execution, record: &Value, request: &Value) -> Result<()> {
    ensure!(
        record.get("request").is_some(),
        "existing run predates idempotent execution; inspect with status"
    );
    ensure!(
        record["request"] == *request,
        "run ID already belongs to a different request"
    );
    if let Some(rev) = &args.candidate {
        if let Some(pinned) = record["requested_candidate_sha"].as_str() {
            ensure!(
                commit(&repo.root, rev)? == pinned,
                "run ID already belongs to a different candidate"
            );
        } else {
            ensure!(
                record["requested_candidate_ref"] == rev.as_str(),
                "different unresolved candidate"
            );
        }
    }
    ensure!(
        record["status"] != "running",
        "previous runner exited without a final result; inspect status and worktree"
    );
    ensure!(
        record["status"] == "awaiting_human_review",
        "{}",
        record["error"]
            .as_str()
            .unwrap_or("previous run did not pass")
    );
    println!(
        "Reused verified candidate {}. Human approval pending.",
        record["candidate_sha"].as_str().unwrap_or("?")
    );
    Ok(())
}
pub fn execute(
    repo: &Repo,
    args: &Execution,
    base: Base<'_>,
    implementer: Option<&str>,
    codex_model: Option<&str>,
    zcode_model: Option<&str>,
) -> Result<ExitCode> {
    let implementer_client = match implementer {
        None => None,
        Some("codex") => Some(Client::Codex),
        Some("zcode") => Some(Client::ZCode),
        Some(other) => bail!("unsupported implementer client {other}"),
    };
    let reviewer_client = match args.reviewer.as_str() {
        "manual" => None,
        "claude" => Some(Client::Claude),
        "zcode" => Some(Client::ZCode),
        other => bail!("unsupported reviewer client {other}"),
    };
    ensure!(
        codex_model.is_none() || implementer_client == Some(Client::Codex),
        "--codex-model 需要 Codex 实现端"
    );
    ensure!(
        zcode_model.is_none()
            || implementer_client == Some(Client::ZCode)
            || reviewer_client == Some(Client::ZCode),
        "--zcode-model 需要 ZCode 实现端或审查端"
    );
    if let Some(model) = zcode_model {
        Profile {
            client: Client::ZCode,
            executable: String::new(),
            model: model.to_owned(),
            check_command: String::new(),
        }
        .validate()?;
    }
    let id = args
        .run_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
    let folder = directory(repo, &id)?;
    let cancellation = Cancellation::new(&repo.data, &id)?;
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&folder)?;
    let path = folder.join("evidence.json");
    println!("Evidence: {}", path.display());
    let file = OpenOptions::new()
        .append(true)
        .create(true)
        .open(folder.join("owner.lock"))?;
    match file.try_lock() {
        Ok(()) => (),
        Err(TryLockError::WouldBlock) => {
            eprintln!("run is busy: {id}");
            return Ok(ExitCode::from(75));
        }
        Err(TryLockError::Error(e)) => return Err(e.into()),
    }
    let base = match base {
        Base::Remote(remote) => repo.dispatched(&args.task, remote)?,
        Base::Local(rev) => commit(&repo.root, rev)?,
    };
    let source = implementer.unwrap_or("candidate");
    ensure!(
        args.require_memory.is_none() || implementer_client.is_some() || reviewer_client.is_some(),
        "required memory needs a model client"
    );
    let mut request = json!({
        "task": args.task, "spec_sha": base, "source": source,
        "reviewer": args.reviewer, "timeout": args.timeout
    });
    if args.shared_memory {
        request["shared_memory"] = json!(true);
    }
    if let Some(id) = args.require_memory {
        request["require_memory"] = json!(id);
    }
    if let Some(m) = codex_model {
        request["codex_model"] = json!(m);
    }
    if let Some(m) = zcode_model {
        request["zcode_model"] = json!(m);
    }
    if args.whole_attempt {
        request["whole_attempt"] = json!(true);
    }
    if let Some(id) = &args.feedback_run {
        request["feedback_run"] = json!(id);
    }
    if path.exists() {
        replay(repo, args, &read(&path)?, &request)?;
        return Ok(ExitCode::SUCCESS);
    }
    let (spec, task) = repo.task(&base, &args.task)?;
    let feedback = retry_feedback(repo, args, &base)?;
    agent::watch_interrupts()?;
    let work = tempfile::Builder::new()
        .prefix("aep-work-")
        .tempdir()?
        .keep();
    let mut record = json!({
        "run_id": id, "task": args.task, "spec_sha": base, "request": request,
        "started_at": Utc::now().to_rfc3339(), "status": "running", "implementation_source": source,
        "requested_candidate_ref": args.candidate, "reviewer": args.reviewer,
        "review_status": "pending", "approved": false, "worktree": work, "phase": "preparing"
    });
    save(&folder, &record)?;
    let mut attached = false;
    let result = (|| -> Result<()> {
        cancellation.check()?;
        let memory = if args.shared_memory {
            record["memory_inputs"] = json!({});
            record["memory_outputs"] = json!({});
            Some(Memory::open(&repo.data)?)
        } else {
            None
        };
        let mut agents = Agents {
            repository: &repo.root,
            work: &work,
            folder: &folder,
            timeout: args.timeout,
            deadline: args
                .whole_attempt
                .then(|| Instant::now() + Duration::from_secs(args.timeout)),
            run: &id,
            implementer: implementer_client.unwrap_or(Client::Codex),
            reviewer: reviewer_client.unwrap_or(Client::Claude),
            zcode_model: zcode_model.map(str::to_owned),
            memory,
            require_memory: args.require_memory,
            cancellation: &cancellation,
        };
        let initial = if let Some(rev) = &args.candidate {
            let sha = commit(&repo.root, rev)?;
            record["requested_candidate_sha"] = json!(sha);
            save(&folder, &record)?;
            sha
        } else {
            base.clone()
        };
        repo.changes(&base, &initial, &task.allowed_paths)?;
        git(
            &repo.root,
            &[
                "worktree",
                "add",
                "--detach",
                work.to_str().context("invalid worktree path")?,
                &initial,
            ],
        )?;
        attached = true;
        cancellation.check()?;
        let key = Key::new(&args.task, &base, &initial);
        if implementer.is_some() {
            record["phase"] = json!("implementing");
            save(&folder, &record)?;
            agents.implement(
                &key,
                &spec,
                &task.allowed_paths,
                match implementer_client {
                    Some(Client::ZCode) => zcode_model,
                    _ => codex_model,
                },
                feedback.as_ref(),
                &mut record,
            )?;
            cancellation.check()?;
            git(&work, &["add", "-A"])?;
            if !git(&work, &["diff", "--cached", "--name-only"])?.is_empty() {
                let title = task
                    .title
                    .as_deref()
                    .context("implementation task requires a title")?;
                git(&work, &["commit", "-m", title])?;
            }
        }
        cancellation.check()?;
        let head = commit(&work, "HEAD")?;
        record["candidate_sha"] = json!(head);
        git(
            &repo.root,
            &["update-ref", &format!("refs/aep/runs/{id}"), &head],
        )?;
        record["changed_files"] = json!(repo.changes(&base, &head, &task.allowed_paths)?);
        let key = Key::new(&args.task, &base, &head);
        // This checkout was created for this run and has never been exposed as a user workspace.
        git(&work, &["reset", "--hard", &head])?;
        git(&work, &["clean", "-ffdx"])?;
        verify_task(&agents, &task, &mut record)?;
        cancellation.check()?;
        if implementer.is_some() && args.shared_memory {
            // Publish cross-run context only after the candidate has passed its checks.
            agents.handoff(&key, &mut record)?;
            save(&folder, &record)?;
        }
        if reviewer_client.is_some() {
            if let Some(budget) = &args.budget {
                budget.check(&mut record, true)?;
            }
            let diff = git(&repo.root, &["diff", "--no-ext-diff", &base, &head])?;
            record["phase"] = json!("reviewing");
            save(&folder, &record)?;
            agents.review(&key, &spec, &diff, &task.criterion_ids(), &mut record)?;
        }
        if let Some(budget) = &args.budget {
            budget.check(&mut record, false)?;
        }
        Ok(())
    })();
    // A stop request and completion are ordered by the same per-run lock.
    let terminal = cancellation.lock()?;
    let result = cancellation.check().and(result);
    if let Err(e) = &result {
        record["status"] = json!(if e.is::<Cancelled>() {
            "cancelled"
        } else {
            "failed"
        });
        record["error"] = json!(format!("{e:#}"));
        if attached {
            record["recovery_worktree"] = json!(work);
        }
    } else {
        record["phase"] = json!("complete");
        record["status"] = json!("awaiting_human_review");
        println!(
            "Checks passed at {}. Human approval pending.",
            record["candidate_sha"].as_str().unwrap_or("?")
        );
    }
    record["finished_at"] = json!(Utc::now().to_rfc3339());
    save(&folder, &record)?;
    drop(terminal);
    let cleanup = if !attached {
        fs::remove_dir(&work).map_err(anyhow::Error::from)
    } else if result.is_ok() {
        git(
            &repo.root,
            &["worktree", "remove", "--force", work.to_str().unwrap()],
        )
        .map(|_| ())
    } else {
        Ok(())
    };
    if let Err(e) = cleanup {
        record["cleanup_error"] = json!(e.to_string());
        save(&folder, &record)?;
        eprintln!("Worktree cleanup needs attention: {e}");
    }
    result?;
    Ok(ExitCode::SUCCESS)
}
fn verify_task(agents: &Agents<'_>, task: &Task, record: &mut Value) -> Result<()> {
    record["phase"] = json!("checking");
    record["check"] = json!(task.check);
    let plan = std::iter::once(("primary", task.criteria.as_slice(), task.check.as_slice()))
        .chain(task.checks.iter().map(|check| {
            (
                check.id.as_str(),
                check.criteria.as_slice(),
                check.command.as_slice(),
            )
        }))
        .collect::<Vec<_>>();
    record["acceptance"] = json!(plan.iter().map(|(id, criteria, command)|
        json!({"id": id, "criteria": criteria, "command": command, "status": "not_run"})
    ).collect::<Vec<_>>());
    save(agents.folder, record)?;
    let combined = agents.folder.join("checks.log");
    for (index, (id, _, command)) in plan.into_iter().enumerate() {
        let individual = (index > 0)
            .then(|| tempfile::NamedTempFile::new_in(agents.folder))
            .transpose()?;
        let log = individual
            .as_ref()
            .map_or(combined.as_path(), |file| file.path());
        let result = agent::logged(
            command,
            agents.work,
            log,
            agents.remaining_timeout()?,
            None,
            None,
            agents.cancellation,
        );
        if index > 0 && log.is_file() {
            let mut output = OpenOptions::new().append(true).open(&combined)?;
            writeln!(output, "\n--- {id} ---")?;
            io::copy(&mut File::open(log)?, &mut output)?;
        }
        let step = &mut record["acceptance"][index];
        if log.is_file() {
            step["log"] = json!("checks.log");
        }
        match result {
            Ok(code) => {
                step["exit_code"] = json!(code);
                step["status"] = json!(if code == 0 { "passed" } else { "failed" });
                record["check_exit_code"] = json!(code);
                save(agents.folder, record)?;
                ensure!(code == 0, "check failed ({code}): {}", command.join(" "));
            }
            Err(error) => {
                step["status"] = json!(if error.is::<Cancelled>() {
                    "cancelled"
                } else {
                    "failed"
                });
                return Err(error);
            }
        }
    }
    Ok(())
}
