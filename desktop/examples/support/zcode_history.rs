use super::{
    Driver,
    https_queue::{label, until},
    project::click,
};
use anyhow::{Context, Result, ensure};
use gpui_kit::test::TestQueryExt;
use std::path::Path;

pub(super) fn exercise(ui: &mut Driver<'_>, repo: &Path) -> Result<()> {
    until(ui, "project settings", 25, |ui| {
        Ok((!ui.absent("project-settings")?).then_some(()))
    })?;
    ui.click("project-settings")?;
    click(ui, &format!("settings-zcode-history-{}", repo.display()))?;
    until(ui, "ZCode history", 30, |ui| {
        Ok((!ui.absent("zcode-history-count")?).then_some(()))
    })?;
    click(ui, "zcode-history-sync")?;
    until(ui, "sync receipt", 60, |ui| {
        let status = ui.element("zcode-history-status")?;
        let msg = status.label().unwrap_or_default();
        ensure!(
            !msg.contains("失败") && !msg.contains("未全部完成"),
            "{msg}"
        );
        Ok(msg.starts_with("同步完成").then_some(()))
    })?;
    ui.checkpoint("zcode-history-synced")?;
    let rows = ui.frame(|w, _| w.elements())?;
    let mut buttons = rows
        .iter()
        .flat_map(|e| e.path().last().map(ToString::to_string))
        .filter(|s| s.starts_with("zcode-history-open-"));
    // Prefer a cleaned project's session to exercise navigation after regrouping.
    let id = rows
        .iter()
        .filter(|e| e.label() == Some("空项目已清理"))
        .filter_map(|e| e.path().last().map(ToString::to_string))
        .find_map(|s| {
            s.strip_prefix("zcode-history-cleanup-")
                .map(|i| format!("zcode-history-open-{i}"))
        })
        .or_else(|| buttons.next())
        .context("no original desktop session to open")?;
    click(ui, &id)?;
    label(ui, "zcode-history-open-status", "已打开 ZCode 任务", 35)?;
    ui.reveal("project-page", &id)?;
    ui.checkpoint("zcode-history-opened")
}

pub(super) fn timeline(
    ui: &mut Driver<'_>,
    repo: &Path,
    work: &str,
    assignment: Option<(&str, usize)>,
) -> Result<()> {
    until(ui, "project settings", 30, |ui| {
        Ok((!ui.absent("project-settings")?).then_some(()))
    })?;
    if let Some((session, stage)) = assignment {
        ui.click("project-settings")?;
        click(ui, &format!("settings-zcode-history-{}", repo.display()))?;
        let edit = format!("assign-{session}");
        until(ui, &edit, 35, |ui| Ok((!ui.absent(&edit)?).then_some(())))?;
        click(ui, &edit)?;
        let item = format!("assign-work-{session}-{work}");
        if !ui.absent(&item)? {
            click(ui, &item)?;
        }
        let i = stage.checked_sub(1).context("stage starts at 1")?;
        click(ui, &format!("assign-stage-{session}-{work}-{i}"))?;
        ui.checkpoint("timeline-linked")?;
    }
    ui.click("project-overview")?;
    click(ui, &format!("project-item-{work}"))?;
    click(ui, "work-timeline-tab")?;
    until(ui, "archived rounds", 30, |ui| {
        Ok((!ui.absent("timeline-title-0")?).then_some(()))
    })?;
    ui.checkpoint("timeline-rounds")?;
    for i in 0.. {
        let detail = format!("timeline-detail-{i}");
        if ui.absent(&detail)? {
            break;
        }
        click(ui, &detail)?;
        for j in 0..2 {
            let open = format!("timeline-open-{i}-{j}");
            if !ui.absent(&open)? {
                click(ui, &open)?;
                label(
                    ui,
                    &format!("timeline-open-status-{i}-{j}"),
                    "已打开 ZCode 任务",
                    40,
                )?;
            }
        }
        ui.reveal("project-page", &detail)?;
        ui.checkpoint(&format!("timeline-attempt-{}", i + 1))?;
        click(ui, &detail)?;
    }
    Ok(())
}
