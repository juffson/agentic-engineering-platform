use super::{
    Args, Driver, handoff_fixture,
    https_pilot::{await_stage, confirm, docker},
    https_queue::{Task, click, dispatch, label, saved_session, select, until},
};
use aep::workspace::{Workspace, git, ident, read};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::Value;
use std::{
    fs,
    io::{self, IsTerminal},
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Deserialize)]
struct Plan {
    site: String,
    second_slot: String,
    recover: Task,
    steady: Task,
}

struct Hold {
    script: PathBuf,
    site: String,
    slot: String,
}

impl Hold {
    fn gate(&self, action: &str) -> Result<()> {
        let output = Command::new(&self.script)
            .env("AEP_PILOT_NAME", &self.site)
            .env("AEP_PILOT_WORKER_SLOT", &self.slot)
            .args(["gate", action])
            .output()?;
        ensure!(
            output.status.success(),
            "Worker gate: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        let _ = self.gate("release");
    }
}

struct Link {
    network: String,
    worker: String,
    detached: bool,
}

impl Link {
    fn restore(&mut self) -> Result<()> {
        if self.detached {
            docker(&["network", "connect", &self.network, &self.worker])?;
            self.detached = false;
        }
        Ok(())
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

fn worker_label(ui: &mut Driver<'_>, worker: &str, state: &str, seconds: u64) -> Result<()> {
    label(
        ui,
        &format!("https-worker-{worker}"),
        &format!("{worker} · {state}"),
        seconds,
    )
}

fn wait_task(
    ui: &mut Driver<'_>,
    task: &Task,
    session: &Path,
    stage: &str,
    seconds: u64,
) -> Result<Value> {
    until(ui, stage, seconds, |_| {
        let state = task.state(session);
        ensure!(
            state["exit_code"].as_i64().is_none_or(|exit| exit == 0),
            "{} failed: {}",
            task.task,
            state["reason"]
        );
        Ok((state["stage"] == stage).then_some(state))
    })
}

fn restore_observer(ui: &mut Driver<'_>, task: &Task, session: &Path) -> Result<()> {
    select(ui, session)?;
    if task.state(session)["stage"] == "completed" {
        return Ok(());
    }
    click(ui, "https-resume")?;
    until(ui, "saved observer", 45, |_| {
        Ok(task.state(session)["exit_code"].is_null().then_some(()))
    })
}

pub(super) fn run(mut args: Args) -> Result<()> {
    ensure!(
        io::stdin().is_terminal(),
        "the Worker rehearsal needs an operator terminal"
    );
    let plan: Plan = serde_json::from_slice(&fs::read(
        std::env::var_os("AEP_STUDIO_LIVE_WORKERS_PLAN")
            .context("set AEP_STUDIO_LIVE_WORKERS_PLAN")?,
    )?)?;
    ident(&plan.site, 40)?;
    ident(&plan.second_slot, 16)?;
    ensure!(
        plan.recover.task != plan.steady.task && plan.recover.target != plan.steady.target,
        "use distinct tasks and isolated targets"
    );
    let ws = Workspace::open(&Path::new(env!("CARGO_MANIFEST_DIR")).join(".."))?;
    for task in [&plan.recover, &plan.steady] {
        ident(&task.task, 63)?;
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
    let first = format!("{}-worker", plan.site);
    let second = format!("{first}-{}", plan.second_slot);
    let control = format!("{}-control", plan.site);
    let private = format!("{}-private", plan.site);
    for worker in [&first, &second, &control] {
        ensure!(
            docker(&[
                "inspect",
                "-f",
                "{{index .Config.Labels \"org.aep.pilot\"}}",
                worker
            ])? == plan.site,
            "the rehearsal containers belong to another site"
        );
    }
    let endpoint = format!("https://{}", docker(&["port", &control, "8443/tcp"])?);
    let parent = ws.data.join("https-studio");
    let output = args
        .output_dir
        .as_ref()
        .context("missing checkpoint directory")?;
    fs::create_dir_all(output)?;
    let output = tempfile::Builder::new()
        .prefix("studio-workers-")
        .tempdir_in(output)?
        .keep();
    args.output_dir = Some(output.clone());
    args.screenshots = true;
    let runner = aep_studio::Runner {
        binary: Some(handoff_fixture::build_runner()?),
        client_bin: None,
    };
    let mut ui = Driver::open_with_runner(&ws, &args, runner)?;
    println!("Worker checkpoints: {}", output.display());
    let result = (|| {
        ui.click("HTTPS 试点")?;
        worker_label(&mut ui, &first, "在线", 30)?;
        worker_label(&mut ui, &second, "在线", 30)?;
        ensure!(
            ui.element("https-queue-endpoint")?.label() == Some(endpoint.as_str()),
            "Studio is connected to another site"
        );
        let resume =
            std::env::var("AEP_STUDIO_LIVE_WORKERS_RESUME").is_ok_and(|value| value == "1");
        let (recover, steady) = if resume {
            let recover = saved_session(&parent, &plan.recover)?;
            let steady = saved_session(&parent, &plan.steady)?;
            ensure!(
                plan.recover.evidence(&recover).join("retry.json").is_file(),
                "resume requires the completed outage drill"
            );
            for (task, path) in [(&plan.recover, &recover), (&plan.steady, &steady)] {
                restore_observer(&mut ui, task, path)?;
            }
            (recover, steady)
        } else {
            label(&mut ui, "https-queue-state", "允许派发", 30)?;
            for id in [&first, &second] {
                label(&mut ui, &format!("https-worker-task-{id}"), "空闲", 30)?;
            }
            let script = ws.root.join("deploy/pilot/site.sh");
            let first_hold = Hold {
                script: script.clone(),
                site: plan.site.clone(),
                slot: String::new(),
            };
            let second_hold = Hold {
                script,
                site: plan.site.clone(),
                slot: plan.second_slot.clone(),
            };
            first_hold.gate("hold")?;
            second_hold.gate("hold")?;
            click(&mut ui, "https-queue-toggle")?;
            label(&mut ui, "https-queue-state", "派发已暂停", 30)?;
            let recover = dispatch(&mut ui, &parent, &plan.recover)?;
            let steady = dispatch(&mut ui, &parent, &plan.steady)?;
            println!(
                "Recovery session: {}\nSteady session: {}",
                recover.display(),
                steady.display()
            );
            ui.checkpoint("01-two-tasks-queued")?;
            click(&mut ui, "https-queue-toggle")?;
            let a = wait_task(&mut ui, &plan.recover, &recover, "remote_checking", 900)?;
            let b = wait_task(&mut ui, &plan.steady, &steady, "remote_checking", 900)?;
            let worker_a = a["worker"].as_str().context("missing recovery Worker")?;
            let worker_b = b["worker"].as_str().context("missing steady Worker")?;
            ensure!(
                (worker_a == first && worker_b == second)
                    || (worker_a == second && worker_b == first),
                "the two tasks did not use distinct enrolled Workers"
            );
            label(
                &mut ui,
                &format!("https-worker-task-{worker_a}"),
                &format!("{} · 运行验收", plan.recover.task),
                30,
            )?;
            label(
                &mut ui,
                &format!("https-worker-task-{worker_b}"),
                &format!("{} · 运行验收", plan.steady.task),
                30,
            )?;
            ui.checkpoint("02-parallel-workers")?;
            let evidence = plan.recover.evidence(&recover);
            fs::copy(
                evidence.join("control-status.json"),
                output.join("before-outage.json"),
            )?;

            let mut link = Link {
                network: private,
                worker: worker_a.into(),
                detached: false,
            };
            docker(&["network", "disconnect", &link.network, &link.worker])?;
            link.detached = true;
            worker_label(&mut ui, worker_a, "离线", 90)?;
            worker_label(&mut ui, worker_b, "在线", 20)?;
            ui.checkpoint("03-one-worker-offline")?;
            let (hold_a, hold_b) = if worker_a == first {
                (&first_hold, &second_hold)
            } else {
                (&second_hold, &first_hold)
            };
            hold_b.gate("release")?;
            let passed = wait_task(&mut ui, &plan.steady, &steady, "publish_confirmation", 180)?;
            ensure!(
                passed["recovered_run"] == b["recovered_run"] && passed["worker"] == worker_b,
                "the steady Run changed during the outage"
            );
            worker_label(&mut ui, worker_a, "离线", 15)?;
            ui.checkpoint("04-peer-reached-approval-offline")?;
            fs::copy(
                plan.steady.evidence(&steady).join("control-status.json"),
                output.join("peer-progress.json"),
            )?;
            link.restore()?;
            hold_a.gate("release")?;
            let recovered = wait_task(
                &mut ui,
                &plan.recover,
                &recover,
                "publish_confirmation",
                240,
            )?;
            ensure!(
                recovered["recovered_run"] != a["recovered_run"]
                    && recovered["worker"] == worker_a
                    && recovered["candidate"] == a["candidate"]
                    && recovered["ticket"] == a["ticket"],
                "recovery changed the pinned Worker or candidate"
            );
            worker_label(&mut ui, worker_a, "在线", 45)?;
            ui.checkpoint("05-worker-returned-same-candidate")?;
            (recover, steady)
        };
        let tasks = [
            ("steady", &plan.steady, &steady),
            ("recovered", &plan.recover, &recover),
        ];
        for (role, task, path) in &tasks {
            let state = task.state(path);
            if state["pr"].is_null() {
                select(&mut ui, path)?;
                let state = await_stage(&mut ui, "待人工确认发布", path, 150)?;
                let sha = state["candidate"]
                    .as_str()
                    .context("missing reviewed candidate")?;
                println!(
                    "Review {role} candidate and Worker evidence in {}",
                    task.evidence(path).display()
                );
                confirm(
                    &mut ui,
                    &format!("publish {}", &sha[..12]),
                    &format!("06-{role}-publication"),
                )?;
                until(&mut ui, "PR receipt", 150, |_| {
                    let state = task.state(path);
                    ensure!(
                        state["exit_code"].is_null(),
                        "publication failed: {}",
                        state["reason"]
                    );
                    Ok(state["pr"].as_u64())
                })?;
            }
        }
        for (role, task, path) in tasks {
            select(&mut ui, path)?;
            if task.state(path)["stage"] != "completed" {
                let state = await_stage(&mut ui, "CI 已通过，待人工确认合并", path, 2800)?;
                let pr = state["pr"].as_u64().context("missing PR")?;
                let sha = state["candidate"]
                    .as_str()
                    .context("missing CI candidate")?;
                println!(
                    "Review {role} PR #{pr} and checks in {}",
                    task.evidence(path).display()
                );
                confirm(
                    &mut ui,
                    &format!("merge {pr} {}", &sha[..12]),
                    &format!("07-{role}-merge"),
                )?;
                await_stage(&mut ui, "已合并并归档", path, 160)?;
            }
            click_archive_checkpoint(&mut ui, role)?;
            let state = task.state(path);
            let evidence = task.evidence(path);
            let pr = read(&evidence.join("github-merged.json"))?;
            let run = state["recovered_run"]
                .as_str()
                .context("missing settled Run")?;
            let worker = read(&evidence.join(format!("worker/runs/{run}/evidence.json")))?;
            ensure!(
                pr["state"] == "MERGED"
                    && pr["headRefOid"] == state["candidate"]
                    && pr["baseRefName"] == task.target
                    && worker["run_id"] == run
                    && worker["candidate_sha"] == state["candidate"]
                    && worker["spec_sha"] == state["spec"],
                "merge or Worker evidence differs from the Studio record"
            );
            fs::copy(
                path.join(&task.task).join("evidence.tar.gz"),
                output.join(format!("{role}-evidence.tar.gz")),
            )?;
        }
        println!(
            "Both Worker tasks verified and archived: {}",
            output.display()
        );
        Ok(())
    })();
    if result.is_err() {
        let _ = ui.checkpoint("failure");
    }
    result
}

fn click_archive_checkpoint(ui: &mut Driver<'_>, role: &str) -> Result<()> {
    ui.reveal("https-pilot-page", "https-archive")?;
    ensure!(
        ui.element("https-archive")?.visible(),
        "Studio did not expose the archive"
    );
    ui.checkpoint(&format!("08-{role}-archive"))
}
