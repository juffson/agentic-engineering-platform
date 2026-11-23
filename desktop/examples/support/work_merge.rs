use super::{Args, Driver, https_queue::until, project::click, stage_lifecycle::open_item};
#[path = "../../../tests/support/work_item_merge.rs"]
mod fixture;
use aep::{
    project::{Library, merge},
    workspace::{Workspace, commit},
};
use anyhow::{Result, ensure};
use std::fs;

pub(super) fn run(args: &Args) -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let f = fixture::create(tmp.path())?;
    let ws = Workspace::open(&f.repo)?;
    let manifest = fs::read(f.delivery.directory(&f.home).join("manifest.json"))?;
    let timeline = fs::read(f.delivery.directory(&f.home).join("timeline.json"))?;
    let mut ui = Driver::open(&ws, args)?;
    ui.click("back-to-project")?;
    open_item(&mut ui, &f.delivery.work)?;
    click(&mut ui, "work-timeline-tab")?;
    let entry = format!("delivery-merge-{}", f.delivery.run);
    until(&mut ui, "归档入口", 30, |ui| {
        Ok((!ui.absent(&entry)?).then_some(()))
    })?;
    click(&mut ui, &entry)?;
    until(&mut ui, "Diff 预览", 30, |ui| {
        Ok((!ui.absent("work-merge-review")?).then_some(()))
    })?;
    ensure!(
        ui.element("work-merge-selected-target")?.label() == Some("target"),
        "默认目标错误"
    );
    let candidate = merge::candidate(&f.home, &f.delivery)?;
    let branches = merge::branches(&candidate)?;
    for target in ["main", "target"] {
        let index = branches.iter().position(|b| b.name == target).unwrap();
        click(&mut ui, &format!("work-merge-target-{index}"))?;
        until(&mut ui, "切换目标", 30, |ui| {
            Ok(ui
                .element("work-merge-selected-target")
                .is_ok_and(|e| e.label() == Some(target))
                .then_some(()))
        })?;
        click(&mut ui, "work-merge-refresh")?;
        until(&mut ui, "刷新后保留目标", 30, |ui| {
            Ok(ui
                .element("work-merge-selected-target")
                .is_ok_and(|e| e.label() == Some(target))
                .then_some(()))
        })?;
    }
    ui.checkpoint("work-merge-preview")?;
    click(&mut ui, "work-merge-file-0")?;
    ui.checkpoint("work-merge-file-diff")?;
    click(&mut ui, "work-merge-review")?;
    ui.checkpoint("work-merge-confirmation")?;
    click(&mut ui, "work-merge-confirm")?;
    let copy = format!("merge-copy-{}", f.delivery.run);
    until(&mut ui, "合并回执", 30, |ui| {
        Ok((!ui.absent(&copy)?).then_some(()))
    })?;
    ui.checkpoint("work-merge-receipt")?;
    ensure!(ui.absent(&entry)?, "合并后仍显示合并入口");
    ensure!(commit(&f.repo, "target")? == f.candidate, "目标未快进");
    ensure!(commit(&f.repo, "HEAD")? == f.base, "主检出被修改");
    ensure!(
        fs::read(f.delivery.directory(&f.home).join("manifest.json"))? == manifest,
        "封存清单被修改"
    );
    ensure!(
        fs::read(f.delivery.directory(&f.home).join("timeline.json"))? == timeline,
        "封存时间线被修改"
    );
    ui = Driver::open(&ws, args)?;
    ui.click("back-to-project")?;
    open_item(&mut ui, &f.delivery.work)?;
    click(&mut ui, "work-timeline-tab")?;
    until(&mut ui, "重开回执", 30, |ui| {
        Ok((!ui.absent(&copy)?).then_some(()))
    })?;
    ui.checkpoint("work-merge-reopened")?;
    let lib = Library::open(&f.home)?;
    lib.project().unwrap().selected().unwrap().deliveries[0].snapshot(&f.home)?;
    Ok(())
}
