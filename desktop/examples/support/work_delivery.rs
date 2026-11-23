use super::{Args, Driver, https_queue::until, project::click, stage_lifecycle::open_item};
use aep::{
    project::{
        catalog_dir,
        directory::{DirectoryAction, DirectoryProject},
    },
    studio::Snapshot,
    workspace::{Workspace, read},
};
use aep_studio::stage_lifecycle::Plan;
use anyhow::{Context, Result, ensure};
use std::{fs, path::Path};
fn open(ui: &mut Driver<'_>, plan: &Plan) -> Result<()> {
    open_item(ui, &plan.work)?;
    click(ui, "work-delivery-tab")
}
fn press(ui: &mut Driver<'_>, container: &str, id: &str) -> Result<()> {
    if !ui.element(id)?.visible() {
        ui.reveal(container, id)?;
    }
    ui.click(id)
}
pub(super) fn run(args: &Args, path: &Path) -> Result<()> {
    let plan: Plan = serde_json::from_slice(&fs::read(path)?)?;
    let ws = Workspace::open(&plan.repository)?;
    let catalog = DirectoryProject::resolve(
        &ws,
        &catalog_dir().context("项目库")?,
        None,
        DirectoryAction::Restore,
    )?;
    let project = catalog
        .library
        .projects
        .iter()
        .find(|p| p.work.iter().any(|w| w.id == plan.work))
        .context("项目")?;
    let root = aep_studio::stage_lifecycle::storage(&catalog.home, &project.id, &plan.work)?;
    super::inspect(args, &ws, |ui| {
        open(ui, &plan)?;
        if !ui.absent("delivery-start")? {
            click(ui, &format!("delivery-spec-{}", plan.task))?;
            for (i, (client, model)) in [
                (&plan.first.implementer, &plan.first.implementation_model),
                (&plan.first.reviewer, &plan.first.review_model),
                (&plan.second.implementer, &plan.second.implementation_model),
                (&plan.second.reviewer, &plan.second.review_model),
                (&plan.test.client.label().to_owned(), &plan.test.model),
                (&plan.accept.client.label().to_owned(), &plan.accept.model),
                (&plan.deliver.client.label().to_owned(), &plan.deliver.model),
            ]
            .into_iter()
            .enumerate()
            {
                click(ui, &format!("delivery-role-{i}"))?;
                let client = match client.to_lowercase().as_str() {
                    "codex" => "Codex",
                    "claude" => "Claude",
                    "zcode" => "ZCode",
                    x => anyhow::bail!("unknown client {x}"),
                };
                click(ui, &format!("delivery-client-{client}"))?;
                ui.fill_project_field("delivery-model-input", "本轮模型 ID", model)?;
            }
            ui.reveal("project-page", "delivery-start")?;
            ui.checkpoint("selected-spec-and-models")?;
            click(ui, "delivery-start")?;
        }
        let mut dirs = fs::read_dir(&root)?.collect::<std::io::Result<Vec<_>>>()?;
        dirs.sort_by_key(|e| e.file_name());
        let dir = dirs
            .iter()
            .rev()
            .map(|e| e.path())
            .find(|p| p.join("launch.json").is_file())
            .context("启动记录")?;
        let saved: Plan = serde_json::from_slice(&fs::read(dir.join("launch.json"))?)?;
        ensure!(
            saved.work == plan.work
                && saved.task == plan.task
                && saved.spec_sha.is_some()
                && serde_json::to_value(&saved.first)? == serde_json::to_value(&plan.first)?
                && serde_json::to_value(&saved.second)? == serde_json::to_value(&plan.second)?
                && [
                    (&saved.test, &plan.test),
                    (&saved.accept, &plan.accept),
                    (&saved.deliver, &plan.deliver)
                ]
                .iter()
                .all(|(a, b)| a.client == b.client && a.model == b.model),
            "启动计划不符"
        );
        let output = dir.join("output");
        if let Some(expected) = &args.retry_failure {
            let failure = read(&output.join("failure.json"))?;
            ensure!(
                failure["run"]
                    .as_str()
                    .or_else(|| failure["phase"].as_str())
                    == Some(expected.as_str()),
                "要重试的执行与失败回执不符"
            );
            let reason = failure["reason"].as_str().context("失败原因")?;
            ensure!(
                ui.element("delivery-failure-reason")?.label() == Some(reason),
                "重开后未显示原失败原因"
            );
            ui.reveal("project-page", "delivery-failed-step")?;
            ui.checkpoint("reopened-failed-step")?;
            ui.reveal("project-page", "delivery-resume")?;
            ui.checkpoint("retry-failed-step")?;
            click(ui, "delivery-resume")?;
        }
        for (phase, kind) in [
            ("relay", "decision"),
            ("test", "transfer"),
            ("accept", "transfer"),
            ("deliver", "transfer"),
            ("deliver", "archive"),
        ] {
            let sub = output.join(phase);
            let consumed = if kind == "decision" {
                sub.join("second-run.json")
            } else if kind == "archive" {
                sub.join("archive.json")
            } else {
                sub.join("confirmed.json")
            };
            if consumed.exists() {
                continue;
            }
            // Open a fresh operator window while the detached command is running.
            *ui = Driver::open(&ws, ui.args)?;
            open(ui, &plan)?;
            let gate = format!("delivery-gate-{phase}");
            ui.checkpoint(&format!("{phase}-{kind}-restored"))?;
            until(ui, "等待原执行确认", 1000, |ui| {
                let ready = read(&sub.join("waiting.json"))
                    .ok()
                    .is_some_and(|v| v["kind"] == kind);
                ensure!(
                    ui.absent("delivery-resume")?,
                    "后台流程停止，见 {}",
                    dir.join("runner.log").display()
                );
                if ready && ui.absent(&gate)? && ui.absent("delivery-status")? {
                    anyhow::bail!("重开后不在开发全流程页面");
                }
                Ok((ready && !ui.absent(&gate)?).then_some(()))
            })?;
            ui.reveal("project-page", &gate)?;
            ui.checkpoint(&format!("{phase}-{kind}"))?;
            click(ui, &gate)?;
            if kind == "decision" {
                let id = fs::read_to_string(sub.join("relay-id"))?;
                let first = read(&sub.join("first-run.json"))?;
                let notes = Snapshot::load(&ws).notes;
                let q = notes
                    .iter()
                    .find(|n| n["kind"] == "open_question" && n["run_id"] == first["run_id"])
                    .context("问题")?["id"]
                    .as_i64()
                    .context("编号")?;
                let resolve = format!("relay-resolve-{q}");
                if ui.absent(&resolve)? {
                    press(ui, "relay-list", &id)?;
                }
                until(ui, "修订问题", 30, |ui| {
                    Ok((!ui.absent(&resolve)?).then_some(()))
                })?;
                press(ui, "relay-detail", &resolve)?;
                ui.replace_focused(&plan.decision)?;
                ui.click("save-note")?;
                let decision = until(ui, "决策版本", 30, |_| {
                    Ok(Snapshot::load(&ws)
                        .notes
                        .iter()
                        .find(|n| n["replaces"] == q && n["body"] == plan.decision)
                        .and_then(|n| n["id"].as_i64()))
                })?;
                let key = format!("relay-continue-{decision}");
                until(ui, "继续第二轮", 30, |ui| {
                    Ok((!ui.absent(&key)?).then_some(()))
                })?;
                press(ui, "relay-detail", &key)?;
            } else if kind == "transfer" {
                let req = read(&sub.join("input.json"))?;
                let key = format!(
                    "confirm-stage-transfer-{}",
                    req["source_run"].as_str().context("来源执行")?
                );
                until(ui, "确认交接", 30, |ui| {
                    Ok((!ui.absent(&key)?).then_some(()))
                })?;
                click(ui, &key)?;
            } else {
                ensure!(
                    ui.element("project-next")?.label() == Some("确认交付并归档"),
                    "归档步骤不符"
                );
                click(ui, "project-next")?;
            }
        }
        *ui = Driver::open(&ws, ui.args)?;
        open(ui, &plan)?;
        until(ui, "核验归档", 120, |ui| {
            Ok((!ui.absent("delivery-result")?).then_some(()))
        })?;
        ui.reveal("project-page", "delivery-result")?;
        ui.checkpoint("reopened-verified")?;
        let verified = read(&output.join("verified.json"))?;
        let shot = Path::new(verified["screenshots"][0].as_str().context("截图索引")?)
            .file_name()
            .and_then(|s| s.to_str())
            .context("截图文件名")?;
        for (key, name, expected) in [
            ("delivery-proof-0", "verified-report", "verified.json"),
            ("delivery-shot-0", "screenshot-preview", shot),
        ] {
            ui.reveal("project-page", key)?;
            click(ui, key)?;
            ensure!(
                ui.element("delivery-preview")?.label() == Some(expected),
                "预览与所选文件不匹配 {expected}"
            );
            ui.reveal("project-page", "delivery-preview")?;
            ui.checkpoint(name)?;
        }
        ensure!(verified["verified"] == true, "未通过核验");
        aep::project::save_json(
            args.output_dir.as_deref().context("output")?,
            "studio.json",
            &serde_json::json!({"job":dir,"report":verified}),
        )?;
        Ok(())
    })
}
