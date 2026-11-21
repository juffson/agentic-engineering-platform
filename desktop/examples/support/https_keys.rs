use super::{
    Args, Driver, LiveKeyStep, handoff_fixture,
    https_pilot::{confirm, confirm_with, sessions},
    https_queue::{Task, dispatch_selected, select, until},
};
use aep::{
    deploy_keys::{RollingRotation, WorkerKeys},
    workspace::{Workspace, ident, read},
};
use aep_studio::rolling_gate::{Action, Gate};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    fs,
    io::{self, IsTerminal, Write},
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant},
};

fn rolling_session() -> Option<std::path::PathBuf> {
    std::env::var_os("AEP_STUDIO_ROLLING_SESSION").map(Into::into)
}

fn read_gate(path: &Path) -> Option<Gate> {
    serde_json::from_reader(fs::File::open(path).ok()?).ok()
}

fn request_approval(
    root: &Path,
    repository: &str,
    worker: &str,
    binding: String,
    action: Action,
) -> Result<()> {
    let mut gate = Gate {
        id: uuid::Uuid::new_v4().to_string(),
        repository: repository.into(),
        worker: worker.into(),
        binding,
        action,
    };
    ensure!(gate.valid(), "invalid Studio approval");
    let path = root.join("gate.json");
    if let Some(previous) = read_gate(&path).filter(|old| old.valid() && old.same_scope(&gate)) {
        gate = previous;
    }
    let already_confirmed = fs::read_dir(root.join("evidence/approvals"))
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| {
            read_gate(&entry.path()).is_some_and(|old| old.valid() && old.same_scope(&gate))
        });
    if !already_confirmed {
        write_json(&path, &gate)?;
        loop {
            ensure!(
                !root.join("pause-request").exists(),
                "Studio paused at approval"
            );
            if let Some(saved) = read_gate(&gate.receipt(root)) {
                ensure!(saved == gate, "Studio approval scope changed");
                break;
            }
            thread::sleep(Duration::from_millis(180));
        }
    } else {
        println!("Reconciled a previously confirmed Studio action for {worker}");
    }
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let temp = tempfile::NamedTempFile::new_in(path.parent().context("missing path parent")?)?;
    serde_json::to_writer(temp.as_file(), value)?;
    temp.persist(path)?;
    Ok(())
}

fn pause(ui: &mut Driver<'_>) -> Result<()> {
    ui.cx.advance_clock(Duration::from_millis(500));
    ui.settle()?;
    thread::sleep(Duration::from_millis(250));
    Ok(())
}

fn click(ui: &mut Driver<'_>, id: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(45);
    while ui.absent(id)? {
        ensure!(Instant::now() < deadline, "Studio did not show {id}");
        pause(ui)?;
    }
    ui.reveal("https-pilot-page", id)?;
    ui.click(id)
}

fn complete(
    ui: &mut Driver<'_>,
    status: &impl Fn() -> Result<WorkerKeys>,
    before: &WorkerKeys,
) -> Result<WorkerKeys> {
    let after = receipt(ui, status, before)?;
    let operation = after
        .last
        .as_ref()
        .context("missing Worker operation receipt")?;
    ensure!(
        operation.error.is_none(),
        "{}",
        operation.error.as_deref().unwrap_or_default()
    );
    Ok(after)
}

fn receipt(
    ui: &mut Driver<'_>,
    status: &impl Fn() -> Result<WorkerKeys>,
    before: &WorkerKeys,
) -> Result<WorkerKeys> {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        pause(ui)?;
        let after = status()?;
        if after.last != before.last && after.request.is_none() {
            return Ok(after);
        }
        ensure!(
            Instant::now() < deadline,
            "Studio Deploy Key action did not finish"
        );
    }
}

fn checkpoint(
    ui: &mut Driver<'_>,
    output: &Path,
    step: &str,
    worker: &str,
    keys: &WorkerKeys,
) -> Result<()> {
    ui.reveal("https-pilot-page", &format!("https-key-scope-{worker}"))?;
    ui.checkpoint(step)?;
    fs::write(
        output.join(format!("{step}-keys.json")),
        serde_json::to_vec_pretty(keys)?,
    )?;
    Ok(())
}

pub(super) fn run(mut args: Args) -> Result<()> {
    let worker = args.https_key_worker.clone().context("missing Worker")?;
    ident(&worker, 63)?;
    let ws = Workspace::open(
        &args
            .https_remote_repo
            .as_ref()
            .context("missing repository")?
            .canonicalize()?,
    )?;
    let dir = args
        .output_dir
        .as_ref()
        .context("missing proof directory")?;
    fs::create_dir_all(dir)?;
    let output = tempfile::Builder::new()
        .prefix("studio-keys-")
        .tempdir_in(dir)?
        .keep();
    args.output_dir = Some(output.clone());
    args.screenshots = true;
    let executable = handoff_fixture::build_runner()?;
    let endpoint = std::env::var("AEP_STUDIO_CONTROL_URL")?;
    ensure!(endpoint.starts_with("https://"), "use the live HTTPS site");
    let operator = fs::read_to_string(
        std::env::var_os("AEP_STUDIO_CONTROL_OPERATOR_FILE")
            .context("missing operator token file")?,
    )?;
    let ca = std::env::var_os("AEP_STUDIO_CONTROL_CA");
    let snapshot = || -> Result<Value> {
        let mut command = Command::new(&executable);
        command
            .arg("--repo")
            .arg(&ws.root)
            .args(["control", "--url", &endpoint])
            .env("AEP_CONTROL_OPERATOR_TOKEN", operator.trim())
            .env_remove("AEP_CONTROL_DEVICE_TOKEN");
        if let Some(ca) = &ca {
            command.arg("--ca-cert").arg(ca);
        }
        let result = command.arg("status").output()?;
        ensure!(result.status.success(), "cannot read the live HTTPS site");
        Ok(serde_json::from_slice(&result.stdout)?)
    };
    let status = || -> Result<WorkerKeys> {
        let value = snapshot()?;
        let keys = value
            .pointer(&format!("/data/deploy_keys/{worker}"))
            .context("selected Worker does not manage Deploy Keys")?;
        Ok(serde_json::from_value(keys.clone())?)
    };
    let rotation = || -> Result<Option<RollingRotation>> {
        Ok(serde_json::from_value(
            snapshot()?["data"]["key_rotation"].clone(),
        )?)
    };
    let runner = aep_studio::Runner {
        binary: Some(executable.clone()),
        client_bin: None,
    };
    let mut ui = Driver::open_with_runner(&ws, &args, runner)?;
    ui.click("HTTPS 试点")?;
    ui.click("https-real-mode")?;
    click(&mut ui, "https-connect-queue")?;
    let scope = format!("https-key-scope-{worker}");
    let deadline = Instant::now() + Duration::from_secs(45);
    while ui.absent(&scope)? {
        ensure!(
            Instant::now() < deadline,
            "Studio did not display the selected Worker"
        );
        pause(&mut ui)?;
    }
    let mut state = status()?;
    checkpoint(&mut ui, &output, "01-worker-keys", &worker, &state)?;
    match args.https_key_step {
        Some(LiveKeyStep::Start) => {
            let previous = rotation()?.map(|plan| plan.id);
            click(&mut ui, "https-key-roll-start")?;
            let plan = super::https_queue::until(&mut ui, "a rolling rotation", 30, |_| {
                Ok(rotation()?
                    .filter(|plan| Some(&plan.id) != previous.as_ref() && plan.current().is_some()))
            })?;
            ensure!(
                plan.current().is_some_and(|first| first.worker == worker)
                    && plan.workers.len() >= 2,
                "unexpected rolling Worker order"
            );
            fs::write(
                output.join("rotation.json"),
                serde_json::to_vec_pretty(&plan)?,
            )?;
            ui.reveal("https-pilot-page", "https-key-roll-state")?;
            ui.checkpoint("02-roll-started")?;
            println!("Rolling rotation started. Evidence: {}", output.display());
            return Ok(());
        }
        Some(LiveKeyStep::Stage) => {
            let plan = rotation()?.context("missing rolling rotation")?;
            ensure!(
                plan.current()
                    .is_some_and(|current| current.worker == worker),
                "wrong rolling Worker"
            );
            click(&mut ui, &format!("https-key-prepare-{worker}"))?;
            let after = complete(&mut ui, &status, &state)?;
            ensure!(
                after.inventory.active == state.inventory.active
                    && after.inventory.staged.is_some(),
                "staging changed the original key"
            );
            checkpoint(&mut ui, &output, "02-roll-staged", &worker, &after)?;
            println!("Rolling key staged. Evidence: {}", output.display());
            return Ok(());
        }
        Some(LiveKeyStep::Verify) => {
            click(&mut ui, &format!("https-key-verify-{worker}"))?;
            let after = complete(&mut ui, &status, &state)?;
            ensure!(
                after.verified_at.is_some(),
                "Worker read-only verification failed"
            );
            checkpoint(&mut ui, &output, "03-1-active-verified", &worker, &after)?;
            return Ok(());
        }
        Some(LiveKeyStep::ExpectFailure) => {
            let before = rotation()?.context("missing rolling rotation")?;
            ensure!(
                before
                    .current()
                    .is_some_and(|current| current.worker == worker),
                "wrong rolling Worker"
            );
            click(&mut ui, &format!("https-key-install-{worker}"))?;
            let after = receipt(&mut ui, &status, &state)?;
            ensure!(
                after.inventory == state.inventory
                    && after.last.as_ref().is_some_and(|last| last.error.is_some())
                    && rotation()? == Some(before),
                "failed cutover changed the key or advanced the rotation"
            );
            super::https_queue::label(
                &mut ui,
                &format!("https-key-status-{worker}"),
                "切换失败 · 保留当前密钥",
                30,
            )?;
            checkpoint(
                &mut ui,
                &output,
                "02-roll-failed-key-preserved",
                &worker,
                &after,
            )?;
            fs::write(
                output.join("failure-site-status.json"),
                serde_json::to_vec_pretty(&snapshot()?)?,
            )?;
            println!(
                "Failed cutover preserved the old key and plan. Evidence: {}",
                output.display()
            );
            return Ok(());
        }
        Some(LiveKeyStep::Deliver) => return deliver(&mut ui, &ws, &output, &worker, &snapshot),
        Some(LiveKeyStep::Provision) | None => (),
    }
    if args.https_key_revoke {
        ensure!(
            io::stdin().is_terminal() || rolling_session().is_some(),
            "retirement requires an operator terminal"
        );
        let old = state
            .inventory
            .retiring
            .as_ref()
            .context("no old Deploy Key is awaiting retirement")?;
        let current = state
            .inventory
            .active
            .as_ref()
            .context("missing active key")?;
        ensure!(
            state.verified_at.is_some(),
            "the new Deploy Key has yet to be verified"
        );
        click(&mut ui, &format!("https-key-review-revoke-{worker}"))?;
        ui.checkpoint("02-review-retirement")?;
        if let Some(root) = rolling_session() {
            request_approval(
                &root,
                &state.repository,
                &worker,
                format!(
                    "{}:{}",
                    state.installation,
                    std::env::var("AEP_STUDIO_ROLLING_PLAN")?
                ),
                Action::Revoke {
                    old: old.fingerprint.clone(),
                    replacement: current.fingerprint.clone(),
                },
            )?;
        } else {
            let confirmation = format!("revoke {}", old.fingerprint);
            print!(
                "Active {}. Type {confirmation} to confirm retirement: ",
                current.fingerprint
            );
            io::stdout().flush()?;
            let mut response = String::new();
            io::stdin().read_line(&mut response)?;
            ensure!(
                response.trim() == confirmation,
                "operator declined retirement"
            );
        }
        click(&mut ui, &format!("https-key-confirm-revoke-{worker}"))?;
        let after = complete(&mut ui, &status, &state)?;
        ensure!(
            after.inventory.retiring.is_none() && after.inventory.active == state.inventory.active,
            "retirement changed the active Deploy Key"
        );
        if rotation()?
            .and_then(|plan| plan.current().cloned())
            .is_none()
        {
            click(&mut ui, &format!("https-key-verify-{worker}"))?;
            state = complete(&mut ui, &status, &after)?;
        } else {
            state = after;
        }
        ensure!(
            state.verified_at.is_some(),
            "new Deploy Key failed verification after retirement"
        );
        checkpoint(&mut ui, &output, "03-retired-and-verified", &worker, &state)?;
    } else {
        if state.inventory.active.is_some() {
            click(&mut ui, &format!("https-key-verify-{worker}"))?;
            state = complete(&mut ui, &status, &state)?;
            ensure!(
                state.verified_at.is_some(),
                "the existing Deploy Key did not verify"
            );
            checkpoint(
                &mut ui,
                &output,
                "01-existing-key-verified",
                &worker,
                &state,
            )?;
        }
        // A fresh Worker starts with no key. Configure its first key before rotating.
        let count = if matches!(args.https_key_step, Some(LiveKeyStep::Provision)) {
            usize::from(state.inventory.active.is_none())
        } else if state.inventory.active.is_none() {
            2
        } else {
            1
        };
        for iteration in 1..=count {
            let previous = state.inventory.active.clone();
            if state.inventory.staged.is_none() {
                click(&mut ui, &format!("https-key-prepare-{worker}"))?;
                state = complete(&mut ui, &status, &state)?;
            }
            let staged = state
                .inventory
                .staged
                .clone()
                .context("missing staged Deploy Key")?;
            checkpoint(
                &mut ui,
                &output,
                &format!("02-{iteration}-key-staged"),
                &worker,
                &state,
            )?;
            click(&mut ui, &format!("https-key-install-{worker}"))?;
            state = complete(&mut ui, &status, &state)?;
            ensure!(
                state.inventory.active == Some(staged)
                    && state.inventory.retiring == previous
                    && state.verified_at.is_some(),
                "Worker failed to switch to the verified Deploy Key"
            );
            click(&mut ui, &format!("https-key-verify-{worker}"))?;
            state = complete(&mut ui, &status, &state)?;
            ensure!(
                state.verified_at.is_some(),
                "GitHub read-only verification did not complete"
            );
            checkpoint(
                &mut ui,
                &output,
                &format!("03-{iteration}-active-verified"),
                &worker,
                &state,
            )?;
        }
    }
    println!(
        "Verified Studio Deploy Key operations for {worker}. Evidence: {}",
        output.display()
    );
    Ok(())
}

#[derive(Clone, Copy)]
enum DeliveryPoint {
    Publication,
    Merge,
    Archive,
}

fn reached_delivery(
    ui: &mut Driver<'_>,
    session: &Path,
    seconds: u64,
    point: DeliveryPoint,
) -> Result<Value> {
    until(ui, "the saved delivery checkpoint", seconds, |ui| {
        let state = read(&session.join("state.json")).unwrap_or_default();
        let label = match state["stage"].as_str() {
            Some("publish_confirmation") if matches!(point, DeliveryPoint::Publication) => {
                Some("待人工确认发布")
            }
            Some("merge_confirmation") if !matches!(point, DeliveryPoint::Archive) => {
                Some("CI 已通过，待人工确认合并")
            }
            Some("completed") if state["exit_code"] == 0 && state["archive"] == true => {
                Some("已合并并归档")
            }
            _ => None,
        };
        ensure!(
            state["exit_code"].is_null() || label == Some("已合并并归档"),
            "Saved delivery failed at {}; read {}",
            state["stage"],
            session.join("studio.log").display()
        );
        Ok(label
            .filter(|label| {
                ui.element("https-stage")
                    .is_ok_and(|element| element.label() == Some(label))
            })
            .map(|_| state))
    })
}

fn open_delivery(
    ui: &mut Driver<'_>,
    parent: &Path,
    task: &Task,
    recover: bool,
) -> Result<std::path::PathBuf> {
    if recover {
        let mut existing = sessions(parent).into_iter().filter(|path| {
            let request = read(&path.join("request.json")).unwrap_or_default();
            request["task"]["name"] == task.task && request["branch"]["name"] == task.target
        });
        if let Some(session) = existing.next() {
            ensure!(
                existing.next().is_none(),
                "multiple saved sessions for the same isolated task"
            );
            select(ui, &session)?;
            let prior = until(ui, "the saved task to become available", 80, |ui| {
                let state = task.state(&session);
                if !ui.absent("https-resume")? {
                    click(ui, "https-resume")?;
                    return Ok(Some(Some(state)));
                }
                Ok((state["stage"] == "completed"
                    && state["exit_code"] == 0
                    && state["archive"] == true)
                    .then_some(None))
            })?;
            if let Some(prior) = prior {
                until(ui, "the resumed task observer", 40, |_| {
                    Ok((task.state(&session) != prior).then_some(()))
                })?;
            }
            return Ok(session);
        }
    }
    dispatch_selected(ui, parent, task)
}

fn merge_binding(evidence: &Path, run: &str) -> Result<String> {
    let binding = read(&evidence.join("worker-merge-context.json"))?;
    ensure!(
        binding["acceptance"]["run"] == run && binding["acceptance"]["result"] == "passed",
        "missing Worker CI for this Run"
    );
    Ok(serde_json::to_string(&binding)?)
}

fn deliver(
    ui: &mut Driver<'_>,
    ws: &Workspace,
    output: &Path,
    worker: &str,
    snapshot: &impl Fn() -> Result<Value>,
) -> Result<()> {
    ensure!(
        io::stdin().is_terminal() || rolling_session().is_some(),
        "delivery requires an operator terminal"
    );
    let task = Task {
        task: std::env::var("AEP_STUDIO_LIVE_TASK")?,
        target: std::env::var("AEP_STUDIO_LIVE_TARGET")?,
    };
    ident(&task.task, 63)?;
    ensure!(
        task.target.starts_with("test/"),
        "use an isolated test branch"
    );
    let before = snapshot()?;
    let plan: RollingRotation = serde_json::from_value(before["data"]["key_rotation"].clone())?;
    ensure!(
        plan.current()
            .is_some_and(|current| current.worker != worker),
        "dispatch needs the peer Worker"
    );
    ensure!(
        before["data"]["workers"][worker]["online"] == true,
        "peer Worker is offline"
    );
    fs::write(
        output.join("dispatch-site-status.json"),
        serde_json::to_vec_pretty(&before)?,
    )?;
    let studio = rolling_session();
    let session = open_delivery(ui, &ws.data.join("https-studio"), &task, studio.is_some())?;
    println!("Remote task session: {}", session.display());
    if let Some(root) = &studio {
        let role = std::env::var("AEP_STUDIO_ROLLING_DELIVERY")?;
        ensure!(
            matches!(role.as_str(), "peer" | "switched"),
            "unknown delivery role"
        );
        write_json(
            &root.join(format!("delivery-{role}.json")),
            &serde_json::json!({"session": session, "task": task.task}),
        )?;
    }
    let pending = reached_delivery(ui, &session, 2_800, DeliveryPoint::Publication)?;
    ensure!(
        pending["worker"] == worker && pending["branch"] == task.target,
        "dispatch did not use the expected Worker or branch"
    );
    if let Some(plan) = std::env::var_os("AEP_STUDIO_ROLLING_TASK_PLAN") {
        let expected = read(Path::new(&plan))?;
        ensure!(
            pending["task"] == expected["task"]
                && pending["branch"] == expected["target"]
                && pending["baseline"] == expected["baseline"]
                && pending["spec"] == expected["spec"],
            "the selected Spec or isolated target changed"
        );
    }
    let run = pending["recovered_run"]
        .as_str()
        .context("missing remote Run")?;
    let candidate = pending["candidate"]
        .as_str()
        .filter(|sha| sha.len() == 40)
        .context("missing remote candidate")?;
    let evidence = task.evidence(&session);
    let report = read(&evidence.join("worker/runs").join(run).join("evidence.json"))?;
    ensure!(
        report["candidate_sha"] == candidate
            && report["check_exit_code"] == 0
            && report["implementation_source"] == "codex"
            && report["reviewer"] == "claude"
            && report["usage"]["implementer"]["output_tokens"]
                .as_u64()
                .is_some_and(|n| n > 0)
            && report["usage"]["reviewer"]["output_tokens"]
                .as_u64()
                .is_some_and(|n| n > 0),
        "remote Run did not contain real implementation, checks and review"
    );
    let merge = if pending["stage"] == "publish_confirmation" {
        ui.checkpoint("03-peer-publication-review")?;
        println!(
            "Review {} and the Run report under {}",
            evidence.join("candidate.diff").display(),
            evidence.display()
        );
        if let Some(root) = &studio {
            confirm_with(ui, "04-peer-approved", || {
                request_approval(
                    root,
                    &plan.repository,
                    worker,
                    run.into(),
                    Action::Publish {
                        target: task.target.clone(),
                        candidate: candidate.into(),
                    },
                )
            })?;
        } else {
            confirm(
                ui,
                &format!("publish {}", &candidate[..12]),
                "04-peer-approved",
            )?;
        }
        reached_delivery(ui, &session, 2_800, DeliveryPoint::Merge)?
    } else {
        pending.clone()
    };
    ensure!(
        merge["candidate"] == candidate
            && merge["worker"] == worker
            && merge["recovered_run"] == run,
        "CI scope changed"
    );
    let pr = merge["pr"].as_u64().context("missing GitHub PR")?;
    let completed = if merge["stage"] == "merge_confirmation" {
        ui.reveal("https-pilot-page", "https-check-0")?;
        ui.checkpoint("05-worker-ci-passed")?;
        println!(
            "Review PR #{pr} and {}",
            evidence.join("worker-ci.json").display()
        );
        if let Some(root) = &studio {
            let binding = merge_binding(&evidence, run)?;
            confirm_with(ui, "05-peer-merge", || {
                request_approval(
                    root,
                    &plan.repository,
                    worker,
                    binding,
                    Action::Merge {
                        target: task.target.clone(),
                        candidate: candidate.into(),
                        pr,
                    },
                )
            })?;
        } else {
            confirm(
                ui,
                &format!("merge {pr} {}", &candidate[..12]),
                "05-peer-merge",
            )?;
        }
        reached_delivery(ui, &session, 240, DeliveryPoint::Archive)?
    } else {
        merge
    };
    ensure!(
        completed["worker"] == worker
            && completed["recovered_run"] == run
            && completed["archive"] == true,
        "archive does not belong to the verified Worker Run"
    );
    fs::write(
        output.join("session-state.json"),
        serde_json::to_vec_pretty(&completed)?,
    )?;
    ui.reveal("https-pilot-page", "https-archive")?;
    ui.checkpoint("06-peer-archive")?;
    let cleanup = read(&evidence.join("cleanup.json"))?;
    ensure!(
        cleanup["target_synced"] == true && cleanup["candidate_branch_removed"] == true,
        "incomplete remote cleanup"
    );
    fs::copy(
        session.join(&task.task).join("evidence.tar.gz"),
        output.join("pilot-evidence.tar.gz"),
    )?;
    fs::copy(evidence.join("result.json"), output.join("result.json"))?;
    fs::write(
        output.join("archive-site-status.json"),
        serde_json::to_vec_pretty(&snapshot()?)?,
    )?;
    println!(
        "Verified and archived PR #{pr} on {worker}. Evidence: {}",
        output.display()
    );
    Ok(())
}
