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
}

fn open_item(ui: &mut Driver<'_>, plan: &Plan) -> Result<()> {
    until(ui, "project", 30, |ui| {
        Ok((!ui.absent("project-overview")?).then_some(()))
    })?;
    ui.click("project-overview")?;
    click(ui, &format!("project-item-{}", plan.work))
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
    super::inspect(args, &ws, |ui| {
        open_item(ui, &plan)?;
        if !item()?
            .incoming(from + 1)
            .is_some_and(|t| t.run == plan.source_run)
        {
            click(ui, &format!("project-stage-{from}"))?;
            click(ui, "stage-transfer-review")?;
            let button = format!("confirm-stage-transfer-{}", plan.source_run);
            until(ui, "reviewed source run", 30, |ui| {
                Ok((!ui.absent(&button)?).then_some(()))
            })?;
            ui.reveal("project-page", &button)?;
            ui.checkpoint("source-acceptance")?;
            click(ui, &button)?;
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
            click(ui, &format!("workflow-client-{:?}", plan.client))?;
            click(ui, "workflow-model")?;
            ui.replace_focused(&plan.model)?;
            click(ui, "project-stage-note")?;
            ui.replace_focused(&plan.brief)?;
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
        Ok(())
    })
}
