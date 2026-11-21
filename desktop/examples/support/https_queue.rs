use super::{
    Args, Driver, handoff_fixture,
    https_pilot::{await_stage, choose, confirm, sessions, worker_ci},
};
use aep::workspace::{Workspace, git, read};
use anyhow::{Context, Result, ensure};
use gpui_kit::test::TestQueryExt;
use serde::Deserialize;
use serde_json::Value;
use std::{
    fs,
    io::{self, IsTerminal},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

#[derive(Deserialize)]
struct Plan {
    deliver: Task,
    cancel: Task,
}

#[derive(Deserialize)]
pub(super) struct Task {
    pub task: String,
    pub target: String,
}

impl Task {
    pub(super) fn state(&self, session: &Path) -> Value {
        read(&session.join("state.json")).unwrap_or_default()
    }

    pub(super) fn evidence(&self, session: &Path) -> PathBuf {
        session.join(&self.task).join("evidence")
    }
}

pub(super) fn until<T>(
    ui: &mut Driver<'_>,
    label: &str,
    seconds: u64,
    mut check: impl FnMut(&mut Driver<'_>) -> Result<Option<T>>,
) -> Result<T> {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        ui.cx.advance_clock(Duration::from_secs(1));
        ui.settle()?;
        if let Some(value) = check(ui)? {
            return Ok(value);
        }
        ensure!(Instant::now() < deadline, "timed out waiting for {label}");
        thread::sleep(Duration::from_millis(160));
    }
}

pub(super) fn click(ui: &mut Driver<'_>, id: &str) -> Result<()> {
    until(ui, id, 30, |ui| {
        if ui.absent(id)? {
            return Ok(None);
        }
        if !ui.element(id)?.visible() {
            ui.reveal("https-pilot-page", id)?;
        }
        Ok(ui.element(id)?.visible().then_some(()))
    })?;
    ui.click(id)
}

pub(super) fn label(ui: &mut Driver<'_>, id: &str, text: &str, seconds: u64) -> Result<()> {
    until(ui, text, seconds, |ui| {
        Ok(ui
            .element(id)
            .is_ok_and(|item| item.label() == Some(text))
            .then_some(()))
    })
}

pub(super) fn dispatch(ui: &mut Driver<'_>, parent: &Path, task: &Task) -> Result<PathBuf> {
    let session = dispatch_selected(ui, parent, task)?;
    until(ui, "the paused Run to queue", 120, |_| {
        let state = task.state(&session);
        ensure!(
            state["exit_code"].is_null(),
            "dispatch failed: {}",
            state["reason"]
        );
        if state["stage"] == "queue_wait" {
            ensure!(
                state["task"] == task.task && state["branch"] == task.target,
                "dispatch scope changed"
            );
            return Ok(Some(()));
        }
        Ok(None)
    })?;
    Ok(session)
}

pub(super) fn dispatch_selected(
    ui: &mut Driver<'_>,
    parent: &Path,
    task: &Task,
) -> Result<PathBuf> {
    let before = sessions(parent);
    click(ui, "https-real-mode")?;
    let selected = format!("https-task-{}", task.task);
    let target = format!("https-target-{}", task.target);
    until(ui, &selected, 90, |ui| {
        Ok((!ui.absent(&selected)? && !ui.absent(&target)?).then_some(()))
    })?;
    choose(ui, "https-task-list", &selected)?;
    choose(ui, "https-target-list", &target)?;
    ui.reveal("https-pilot-page", "https-dispatch")?;
    ui.checkpoint("02-task-target-selected")?;
    ui.click("https-dispatch")?;
    until(ui, "Studio to start the selected task", 15, |ui| {
        let created = sessions(parent);
        let mut new = created.difference(&before);
        if let Some(session) = new.next() {
            ensure!(new.next().is_none(), "Studio started multiple tasks");
            return Ok(Some(session.clone()));
        }
        if !ui.absent("https-action-error")? {
            let error = ui.element("https-action-error")?;
            anyhow::bail!(
                "Studio dispatch failed: {}",
                error.label().unwrap_or_default()
            );
        }
        Ok(None)
    })
}

pub(super) fn select(ui: &mut Driver<'_>, session: &Path) -> Result<()> {
    let name = session
        .file_name()
        .context("missing Studio session name")?
        .to_string_lossy();
    let id = format!("https-session-{name}");
    let list = "https-session-list";
    let top = f32::from(ui.element(list)?.bounds().origin.y);
    if !(0. ..=450.).contains(&top) {
        ui.reveal("https-pilot-page", list)?;
    }
    if ui.element(&id)?.visible() {
        ui.click(&id)
    } else {
        choose(ui, "https-session-list", &id)
    }
}

fn verify_order(ui: &mut Driver<'_>, first: &str, second: &str) -> Result<()> {
    let row = |id: &str| format!("https-queue-run-{id}");
    until(ui, "the persisted queue order", 25, |ui| {
        let (Ok(first), Ok(second)) = (ui.element(&row(first)), ui.element(&row(second))) else {
            return Ok(None);
        };
        Ok((first.bounds().origin.y < second.bounds().origin.y).then_some(()))
    })
}

pub(super) fn saved_session(parent: &Path, task: &Task) -> Result<PathBuf> {
    let mut saved = sessions(parent).into_iter().filter(|session| {
        let state = task.state(session);
        state["remote"] == true && state["task"] == task.task && state["branch"] == task.target
    });
    let session = saved
        .next()
        .context("no saved session for the rehearsal task")?;
    ensure!(
        saved.next().is_none(),
        "multiple sessions match the rehearsal task"
    );
    Ok(session)
}

pub(super) fn run(mut args: Args) -> Result<()> {
    ensure!(
        io::stdin().is_terminal(),
        "queue rehearsal needs an operator terminal"
    );
    let plan: Plan = serde_json::from_slice(&fs::read(
        std::env::var_os("AEP_STUDIO_LIVE_QUEUE_PLAN").context("set AEP_STUDIO_LIVE_QUEUE_PLAN")?,
    )?)?;
    ensure!(
        plan.cancel.task != plan.deliver.task && plan.cancel.target != plan.deliver.target,
        "use two distinct tickets and targets"
    );
    let ws = Workspace::open(&Path::new(env!("CARGO_MANIFEST_DIR")).join(".."))?;
    for task in [&plan.cancel, &plan.deliver] {
        aep::workspace::ident(&task.task, 63)?;
        ensure!(
            task.target.starts_with("test/"),
            "use isolated test branches"
        );
        ensure!(
            !git(
                &ws.root,
                &[
                    "ls-remote",
                    "--refs",
                    "origin",
                    &format!("refs/heads/{}", task.target)
                ]
            )?
            .is_empty(),
            "missing isolated target"
        );
    }
    let parent = ws.data.join("https-studio");
    let output = args
        .output_dir
        .as_ref()
        .context("missing checkpoints directory")?;
    fs::create_dir_all(output)?;
    let output = tempfile::Builder::new()
        .prefix("studio-queue-")
        .tempdir_in(output)?
        .keep();
    args.output_dir = Some(output.clone());
    args.screenshots = true;
    let runner = aep_studio::Runner {
        binary: Some(handoff_fixture::build_runner()?),
        client_bin: None,
    };
    let mut ui = Driver::open_with_runner(&ws, &args, runner.clone())?;
    println!("Queue checkpoints: {}", output.display());
    let result = (|| {
        ui.click("HTTPS 试点")?;
        let resume = std::env::var("AEP_STUDIO_LIVE_QUEUE_RESUME").is_ok_and(|value| value == "1");
        let (deliver, cancel) = if resume {
            (
                saved_session(&parent, &plan.deliver)?,
                saved_session(&parent, &plan.cancel)?,
            )
        } else {
            label(&mut ui, "https-queue-state", "允许派发", 30)?;
            let expected =
                std::env::var("AEP_STUDIO_CONTROL_URL").context("set AEP_STUDIO_CONTROL_URL")?;
            ensure!(
                ui.element("https-queue-endpoint")?.label() == Some(expected.as_str()),
                "Studio restored a different site"
            );
            ensure!(
                !ui.frame(|w, _| w.elements().iter().any(|element| {
                    element
                        .path()
                        .last()
                        .is_some_and(|part| format!("{part:?}").contains("https-queue-run-"))
                }))?,
                "the site must be idle before the rehearsal"
            );
            click(&mut ui, "https-queue-toggle")?;
            label(&mut ui, "https-queue-state", "派发已暂停", 30)?;
            ui.checkpoint("01-pause")?;
            let deliver = dispatch(&mut ui, &parent, &plan.deliver)?;
            let cancel = dispatch(&mut ui, &parent, &plan.cancel)?;
            let state = plan.deliver.state(&deliver);
            let deliver_run = state["recovered_run"]
                .as_str()
                .context("missing delivery Run")?
                .to_owned();
            let state = plan.cancel.state(&cancel);
            let cancel_run = state["recovered_run"]
                .as_str()
                .context("missing cancellation Run")?
                .to_owned();
            println!(
                "Delivery session: {}\nCancellation session: {}",
                deliver.display(),
                cancel.display()
            );
            verify_order(&mut ui, &deliver_run, &cancel_run)?;
            click(&mut ui, &format!("https-queue-up-{cancel_run}"))?;
            verify_order(&mut ui, &cancel_run, &deliver_run)?;
            ui.checkpoint("02-reordered")?;

            ui.frame(|w, _| w.remove_window())?;
            ui = Driver::open_with_runner(&ws, &args, runner.clone())?;
            ui.click("HTTPS 试点")?;
            label(&mut ui, "https-queue-state", "派发已暂停", 30)?;
            verify_order(&mut ui, &cancel_run, &deliver_run)?;
            ui.checkpoint("03-reopened")?;
            for session in [&deliver, &cancel] {
                select(&mut ui, session)?;
                click(&mut ui, "https-resume")?;
                until(&mut ui, "the saved observer to restart", 45, |_| {
                    let state = read(&session.join("state.json")).unwrap_or_default();
                    Ok(state["exit_code"].is_null().then_some(()))
                })?;
                await_stage(&mut ui, "等待 HTTPS Worker 领取", session, 90)?;
            }
            click(&mut ui, "https-queue-toggle")?;
            label(&mut ui, "https-queue-state", "允许派发", 30)?;

            let running = await_stage(&mut ui, "远程 Worker 正在验收固定候选", &cancel, 900)?;
            ensure!(
                running["recovered_run"] == cancel_run
                    && plan.deliver.state(&deliver)["stage"] == "queue_wait",
                "the wrong Run advanced first"
            );
            click(&mut ui, &format!("https-queue-cancel-{cancel_run}"))?;
            ui.checkpoint("04-review-cancellation")?;
            click(&mut ui, &format!("https-queue-confirm-{cancel_run}"))?;
            await_stage(&mut ui, "任务已取消，证据已归档", &cancel, 120)?;
            ui.reveal("https-pilot-page", "https-archive")?;
            ensure!(
                ui.element("https-archive")?.visible(),
                "cancelled task has no archive"
            );
            ui.checkpoint("05-cancelled-and-archived")?;
            (deliver, cancel)
        };
        let cancel_state = plan.cancel.state(&cancel);
        let delivery_state = plan.deliver.state(&deliver);
        let cancel_run = cancel_state["recovered_run"]
            .as_str()
            .context("missing cancellation Run")?;
        let deliver_run = delivery_state["recovered_run"]
            .as_str()
            .context("missing delivery Run")?;
        ensure!(
            cancel_state["stage"] == "cancelled" && cancel_state["exit_code"] == 0,
            "the saved cancellation is unfinished"
        );
        let cancel_evidence = plan.cancel.evidence(&cancel);
        let receipt = read(&cancel_evidence.join("cancellation.json"))?;
        let worker = read(
            &cancel_evidence
                .join("worker/runs")
                .join(cancel_run)
                .join("evidence.json"),
        )?;
        ensure!(
            receipt["stage"] == "cancelled"
                && receipt["id"] == cancel_run
                && receipt["events"]
                    .as_array()
                    .and_then(|events| events.last())
                    .is_some_and(|event| event["progress"]["type"] == "exit")
                && worker["status"] == "cancelled"
                && worker["run_id"] == cancel_run
                && !cancel_evidence.join("publication.json").exists(),
            "cancellation did not preserve stopped Worker evidence"
        );
        fs::copy(
            cancel.join(&plan.cancel.task).join("evidence.tar.gz"),
            output.join("cancelled-evidence.tar.gz"),
        )?;

        select(&mut ui, &deliver)?;
        if resume {
            click(&mut ui, "https-resume")?;
            until(&mut ui, "the delivery observer to restart", 45, |_| {
                Ok(plan.deliver.state(&deliver)["exit_code"]
                    .is_null()
                    .then_some(()))
            })?;
            ui.checkpoint("05b-delivery-resumed")?;
        }
        let evidence = plan.deliver.evidence(&deliver);
        let candidate = if delivery_state["pr"].is_null() {
            let pending = await_stage(&mut ui, "待人工确认发布", &deliver, 900)?;
            ensure!(
                pending["recovered_run"] == deliver_run && pending["pr"].is_null(),
                "delivery Run changed or was published early"
            );
            let candidate = pending["candidate"].as_str().context("missing candidate")?;
            println!("Review candidate and reports in {}", evidence.display());
            ui.checkpoint("06-publication-review")?;
            confirm(
                &mut ui,
                &format!("publish {}", &candidate[..12]),
                "07-publication-confirmed",
            )?;
            candidate.to_owned()
        } else {
            delivery_state["candidate"]
                .as_str()
                .context("missing published candidate")?
                .to_owned()
        };
        let pending = await_stage(&mut ui, "CI 已通过，待人工确认合并", &deliver, 2800)?;
        let number = pending["pr"].as_u64().context("missing GitHub PR")?;
        worker_ci(&evidence, &pending)?;
        ensure!(
            pending["candidate"] == candidate && pending["branch"] == plan.deliver.target,
            "the merge confirmation changed the candidate or target"
        );
        ui.checkpoint("08-ci")?;
        println!(
            "Review PR #{number} and its CI receipt in {}",
            evidence.display()
        );
        confirm(
            &mut ui,
            &format!("merge {number} {}", &candidate[..12]),
            "09-merge-confirmed",
        )?;
        await_stage(&mut ui, "已合并并归档", &deliver, 180)?;
        ui.reveal("https-pilot-page", "https-archive")?;
        ensure!(
            ui.element("https-archive")?.visible(),
            "merged task has no archive"
        );
        ui.checkpoint("10-merged-and-archived")?;
        let cleanup = read(&evidence.join("cleanup.json"))?;
        let merge = read(&evidence.join("github-merged.json"))?;
        ensure!(
            merge["state"] == "MERGED"
                && merge["headRefOid"] == candidate
                && cleanup["merge_sha"] == merge["mergeCommit"]["oid"]
                && cleanup["target_synced"] == true
                && cleanup["candidate_branch_removed"] == true,
            "cleanup did not match merged candidate"
        );
        fs::copy(
            deliver.join(&plan.deliver.task).join("evidence.tar.gz"),
            output.join("merged-evidence.tar.gz"),
        )?;
        println!(
            "Verified isolated PR #{number}; two archives saved in {}",
            output.display()
        );
        Ok(())
    })();
    if result.is_err() {
        let _ = ui.checkpoint("failure");
    }
    result
}
