use super::{Args, Driver, https_queue::until, project::click};
use aep::{
    project::{
        Library, catalog_dir,
        directory::{DirectoryAction, DirectoryProject},
        timeline,
    },
    workflow::{Client, attempt_dir},
    workspace::{Workspace, read},
};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
struct Plan {
    repository: PathBuf,
    work: String,
    stage: usize,
    source_run: String,
    client: Client,
    model: String,
    brief: String,
    #[serde(default)]
    finalize_delivery: bool,
    #[serde(default)]
    operator_gates: bool,
}

fn open_item(ui: &mut Driver<'_>, plan: &Plan) -> Result<()> {
    super::stage_lifecycle::open_item(ui, &plan.work)
}
pub(super) fn run(args: &Args, path: &Path) -> Result<()> {
    let plan: Plan = serde_json::from_slice(&fs::read(path)?)?;
    let ws = Workspace::open(&plan.repository)?;
    let project = DirectoryProject::resolve(
        &ws,
        &catalog_dir().context("catalog")?,
        None,
        DirectoryAction::Restore,
    )?;
    let p = project.library.project().context("active project")?;
    let home = &project.home;
    let item = || -> Result<aep::project::Work> {
        Library::open(home)?
            .projects
            .into_iter()
            .find(|s| s.id == p.id)
            .and_then(|p| p.work.into_iter().find(|w| w.id == plan.work))
            .context("work item")
    };
    let dir = args.output_dir.as_ref().context("output directory")?;
    let from = plan.stage.checked_sub(1).context("stage starts at one")?;
    if plan.finalize_delivery {
        let work = item()?;
        let stages = work.template.stages();
        ensure!(
            plan.stage.checked_add(1) == Some(stages.len())
                && stages
                    .last()
                    .is_some_and(|s| s.role == aep::workflow::Role::Deliver),
            "只能归档最后一个交付阶段"
        );
    }
    super::inspect(args, &ws, |ui| {
        open_item(ui, &plan)?;
        if !item()?
            .incoming(from + 1)
            .is_some_and(|t| t.run == plan.source_run)
        {
            click(ui, &format!("project-stage-{from}"))?;
            click(ui, "stage-transfer-review")?;
            let button = format!("confirm-stage-transfer-{}", plan.source_run);
            let show_button = until(ui, "reviewed source run", 30, |ui| {
                if !ui.absent(&button)? {
                    return Ok(Some(true));
                }
                Ok((plan.operator_gates && item()?.incoming(from + 1).is_some()).then_some(false))
            })?;
            if show_button {
                ui.reveal("project-page", &button)?;
                ui.checkpoint("source-acceptance")?;
            }
            if plan.operator_gates {
                super::stage_lifecycle::wait_for_operator(
                    &format!(
                        "在事项 {} 的第 {} 阶段确认执行 {} 交接到下一阶段",
                        plan.work, plan.stage, plan.source_run
                    ),
                    || {
                        let work = item()?;
                        if let Some(t) = work.incoming(from + 1) {
                            ensure!(t.run == plan.source_run, "已确认不同的来源执行");
                            return Ok(true);
                        }
                        Ok(false)
                    },
                )?;
            } else {
                click(ui, &button)?;
            }
        }
        let transfer = item()?
            .incoming(from + 1)
            .cloned()
            .context("missing confirmation")?;
        ensure!(
            transfer.run == plan.source_run && transfer.work == plan.work && transfer.from == from,
            "wrong confirmation"
        );
        fs::write(
            dir.join("confirmed.json"),
            serde_json::to_vec_pretty(&transfer)?,
        )?;
        *ui = Driver::open(&ws, ui.args)?;
        open_item(ui, &plan)?;
        click(ui, &format!("project-stage-{}", from + 1))?;
        let mut existing = None;
        for a in item()?
            .attempts
            .iter()
            .rev()
            .filter(|a| a.stage == from + 1)
        {
            let path = attempt_dir(home, &p.id, &plan.work, &a.id);
            let req = read(&path.join("request.json"))?;
            if req["stage_transfer"]["run"] == plan.source_run
                && req["profile"]["client"] == serde_json::to_value(plan.client)?
                && req["profile"]["model"] == plan.model
                && req["brief"] == plan.brief
            {
                existing = Some(a.id.clone());
                break;
            }
        }
        let id = if let Some(id) = existing {
            id
        } else {
            ui.reveal("project-page", "project-stage-note")?;
            click(ui, "project-stage-note")?;
            ui.replace_focused(&plan.brief)?;
            click(ui, &format!("workflow-client-{:?}", plan.client))?;
            ui.fill_project_field("workflow-model", "模型 ID · 留空使用 CLI 默认", &plan.model)?;
            ui.reveal("project-page", "workflow-run")?;
            click(ui, "workflow-run")?;
            item()?
                .current_attempt
                .get(&(from + 1))
                .cloned()
                .context("new stage not started")?
        };
        let path = attempt_dir(home, &p.id, &plan.work, &id);
        let ev = until(ui, "destination stage", 930, |_| {
            let ev = read(&path.join("evidence.json")).unwrap_or_default();
            Ok(ev["finished_at"].is_string().then_some(ev))
        })?;
        ensure!(ev["status"] == "succeeded", "stage {}: {}", id, ev["error"]);
        ensure!(
            ev["read_stage_transfer"] == serde_json::to_value(&transfer)?
                && ev["profile"]["model"] == plan.model,
            "wrong model or incoming delivery"
        );
        ensure!(
            fs::read_to_string(path.join("prompt.txt"))?.contains(&transfer.context()),
            "missing delivery in prompt"
        );
        if let Some(sha) = &transfer.candidate
            && transfer.repository.as_ref() == Some(&ws.root)
        {
            ensure!(
                aep::workspace::commit(
                    Path::new(ev["workspace"].as_str().context("workspace")?),
                    "HEAD"
                )? == *sha,
                "testing the wrong commit"
            );
        }
        fs::write(
            dir.join("destination.json"),
            serde_json::to_vec_pretty(&ev)?,
        )?;
        *ui = Driver::open(&ws, ui.args)?;
        open_item(ui, &plan)?;
        click(ui, "work-timeline-tab")?;
        let history = timeline::load(home, &p.id, &item()?)?;
        ensure!(
            history.errors.is_empty() && history.stage_transfers.iter().any(|h| h == &transfer),
            "history is incomplete"
        );
        for (name, run) in [("source", &plan.source_run), ("destination", &id)] {
            let (i, r) = history
                .rounds
                .iter()
                .enumerate()
                .find(|(_, r)| r.run == *run)
                .context("attempt not present")?;
            if name == "destination" {
                ensure!(
                    r.read_stage_transfer.as_ref() == Some(&transfer) && r.stage == Some(from + 1),
                    "destination unlinked"
                );
            }
            let label = format!("timeline-transfer-{name}-{i}");
            until(ui, &label, 30, |ui| Ok((!ui.absent(&label)?).then_some(())))?;
            ui.reveal("project-page", &label)?;
            ui.checkpoint(&format!("reopened-{name}"))?;
        }
        fs::write(
            dir.join("timeline.json"),
            serde_json::to_vec_pretty(&history)?,
        )?;
        if plan.finalize_delivery {
            if item()?.stage == from + 1 {
                *ui = Driver::open(&ws, ui.args)?;
                open_item(ui, &plan)?;
                click(ui, &format!("project-stage-{}", from + 1))?;
                if !plan.operator_gates || !ui.absent("project-next")? {
                    ensure!(
                        ui.element("project-next")?.label() == Some("确认交付并归档"),
                        "缺少归档确认"
                    );
                    ui.reveal("project-page", "project-next")?;
                    ui.checkpoint("delivery-confirmation")?;
                }
                if plan.operator_gates {
                    super::stage_lifecycle::wait_for_operator(
                        &format!(
                            "在事项 {} 的交付阶段核对执行 {} 并点击「确认交付并归档」",
                            plan.work, id
                        ),
                        || {
                            let work = item()?;
                            ensure!(
                                work.current_attempt.get(&(from + 1)) == Some(&id),
                                "交付执行已切换"
                            );
                            Ok(work
                                .deliveries
                                .iter()
                                .any(|r| r.run == id && r.accepted_run == plan.source_run))
                        },
                    )?;
                } else {
                    click(ui, "project-next")?;
                }
            }
            let closed = item()?;
            ensure!(
                closed.stage == closed.template.stages().len(),
                "事项尚未交付"
            );
            let receipt = closed
                .deliveries
                .iter()
                .find(|d| d.run == id && d.accepted_run == plan.source_run)
                .context("交付归档未关联到验收")?;
            *ui = Driver::open(&ws, ui.args)?;
            open_item(ui, &plan)?;
            click(ui, "work-timeline-tab")?;
            let button = format!("delivery-snapshot-{}", id);
            until(ui, "归档索引", 30, |ui| {
                Ok((!ui.absent(&button)?).then_some(()))
            })?;
            click(ui, &button)?;
            let marker = "delivery-snapshot-open";
            ensure!(!ui.absent(marker)?, "无法打开已归档的时间线");
            ui.reveal("project-page", marker)?;
            ui.checkpoint("reopened-delivery-archive")?;
            let snapshot = receipt.snapshot(home)?;
            ensure!(
                snapshot.rounds.len() == history.rounds.len()
                    && snapshot.stage_transfers == history.stage_transfers,
                "重开后的归档缺少历史记录"
            );
            for (i, row) in snapshot.rounds.iter().enumerate() {
                ensure!(
                    !ui.absent(&format!("timeline-title-{i}"))?,
                    "归档未显示执行 {}",
                    row.run
                );
            }
            fs::write(
                dir.join("archive.json"),
                serde_json::to_vec_pretty(
                    &serde_json::json!({"directory":receipt.directory(home),"receipt":receipt}),
                )?,
            )?;
        }
        Ok(())
    })
}
