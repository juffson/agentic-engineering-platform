use crate::{
    Execution,
    agent::{self, Agents},
    git::{Repo, Task, commit, git, ident},
    memory::{Key, Memory},
};
use anyhow::{Context, Result, ensure};
use chrono::Utc;
use serde_json::{Value, json};
use std::{
    fs::{self, DirBuilder, OpenOptions, TryLockError},
    io::Write,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    process::ExitCode,
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
    model: Option<&str>,
) -> Result<ExitCode> {
    let id = args
        .run_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
    let folder = directory(repo, &id)?;
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
    let mut request = json!({
        "task": args.task, "spec_sha": base, "source": source,
        "reviewer": args.reviewer, "timeout": args.timeout
    });
    if args.shared_memory {
        request["shared_memory"] = json!(true);
    }
    if let Some(m) = model {
        request["codex_model"] = json!(m);
    }
    if path.exists() {
        replay(repo, args, &read(&path)?, &request)?;
        return Ok(ExitCode::SUCCESS);
    }
    let (spec, task) = repo.task(&base, &args.task)?;
    agent::watch_interrupts()?;
    let work = tempfile::Builder::new()
        .prefix("aep-work-")
        .tempdir()?
        .keep();
    let mut record = json!({
        "run_id": id, "task": args.task, "spec_sha": base, "request": request,
        "started_at": Utc::now().to_rfc3339(), "status": "running", "implementation_source": source,
        "requested_candidate_ref": args.candidate, "reviewer": args.reviewer,
        "review_status": "pending", "approved": false, "worktree": work
    });
    save(&folder, &record)?;
    let mut attached = false;
    let result = (|| -> Result<()> {
        let memory = if args.shared_memory {
            record["memory_inputs"] = json!({});
            record["memory_outputs"] = json!({});
            Some(Memory::open(&repo.data)?)
        } else {
            None
        };
        let mut agents = Agents {
            work: &work,
            folder: &folder,
            timeout: args.timeout,
            run: &id,
            memory,
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
        let key = Key::new(&args.task, &base, &initial);
        if implementer.is_some() {
            agents.implement(&key, &spec, &task.allowed_paths, model, &mut record)?;
            git(&work, &["add", "-A"])?;
            if !git(&work, &["diff", "--cached", "--name-only"])?.is_empty() {
                let title = task
                    .title
                    .as_deref()
                    .context("implementation task requires a title")?;
                git(&work, &["commit", "-m", title])?;
            }
        }
        let head = commit(&work, "HEAD")?;
        record["candidate_sha"] = json!(head);
        git(
            &repo.root,
            &["update-ref", &format!("refs/aep/runs/{id}"), &head],
        )?;
        record["changed_files"] = json!(repo.changes(&base, &head, &task.allowed_paths)?);
        let key = Key::new(&args.task, &base, &head);
        if implementer.is_some() {
            agents.handoff(&key, &mut record)?;
        }
        // This checkout was created for this run and has never been exposed as a user workspace.
        git(&work, &["reset", "--hard", &head])?;
        git(&work, &["clean", "-ffdx"])?;
        verify_task(&agents, &task, &mut record)?;
        if args.reviewer == "claude" {
            let diff = git(&repo.root, &["diff", "--no-ext-diff", &base, &head])?;
            agents.review(&key, &spec, &diff, &mut record)?;
        }
        record["status"] = json!("awaiting_human_review");
        println!("Checks passed at {head}. Human approval pending.");
        Ok(())
    })();
    if let Err(e) = &result {
        record["status"] = json!("failed");
        record["error"] = json!(format!("{e:#}"));
        if attached {
            record["recovery_worktree"] = json!(work);
        }
    }
    record["finished_at"] = json!(Utc::now().to_rfc3339());
    save(&folder, &record)?;
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
    record["check"] = json!(task.check);
    save(agents.folder, record)?;
    let rc = agent::logged(
        &task.check,
        agents.work,
        &agents.folder.join("checks.log"),
        agents.timeout,
        None,
        None,
    )?;
    record["check_exit_code"] = json!(rc);
    ensure!(rc == 0, "check failed ({rc}): {}", task.check.join(" "));
    Ok(())
}
