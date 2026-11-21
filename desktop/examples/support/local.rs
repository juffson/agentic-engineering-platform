use super::{Args, Clients, Driver, handoff_fixture, https_queue::until};
use aep::workspace::{git, read};
use anyhow::{Context, Result, ensure};
use std::fs;

fn click(ui: &mut Driver<'_>, id: &str) -> Result<()> {
    ui.reveal("local-page", id)?;
    ui.click(id)
}

fn available(ui: &mut Driver<'_>, id: &str) -> Result<()> {
    until(ui, id, 60, |ui| Ok((!ui.absent(id)?).then_some(())))
}

pub(super) fn run(args: &Args) -> Result<()> {
    let root = tempfile::tempdir()?;
    let ws = handoff_fixture::prepare(root.path(), Clients::Fixture)?;
    git(&ws.root, &["branch", "-M", "main"])?;
    git(&ws.root, &["remote", "remove", "origin"])?;
    let runner = handoff_fixture::studio_runner(root.path())?;
    // This rehearsal's branches, fixtures and delivery are entirely local.
    fs::write(root.path().join("bin/gh"), "#!/bin/sh\nexit 88\n")?;
    let fail = root.path().join("bin/fail-check");
    fs::write(&fail, "")?;
    let mut ui = Driver::open_with_runner(&ws, args, runner.clone())?;
    ui.click("本地任务")?;
    for (id, value) in [
        ("local-ticket", "local-check"),
        ("local-task", "handoff"),
        ("local-spec", "HEAD"),
    ] {
        click(&mut ui, id)?;
        ui.input(value)?;
    }
    click(&mut ui, "local-plan")?;
    available(&mut ui, "local-approve-plan")?;
    ui.reveal("local-page", "local-approve-plan")?;
    ui.checkpoint("local-plan-approval")?;
    click(&mut ui, "local-approve-plan")?;
    available(&mut ui, "local-run")?;
    click(&mut ui, "local-run")?;
    available(&mut ui, "local-run-reason")?;
    ui.reveal("local-page", "local-run-reason")?;
    ui.checkpoint("local-check-failed")?;

    fs::remove_file(fail)?;
    ui.frame(|window, _| window.remove_window())?;
    ui = Driver::open_with_runner(&ws, args, runner)?;
    ui.click("本地任务")?;
    available(&mut ui, "local-ticket-local-check")?;
    click(&mut ui, "local-ticket-local-check")?;
    available(&mut ui, "local-run")?;
    ui.reveal("local-page", "local-run")?;
    ui.checkpoint("local-reopened-retry")?;
    click(&mut ui, "local-run")?;

    available(&mut ui, "local-approve-delivery")?;
    click(&mut ui, "local-tab-review")?;
    let evidence = ui.element("local-evidence-text")?;
    ensure!(
        evidence
            .label()
            .is_some_and(|text| text.contains("Reviewed and verified")),
        "Studio did not show the independent review"
    );
    ui.checkpoint("local-review-evidence")?;
    click(&mut ui, "local-approve-delivery")?;
    available(&mut ui, "local-merge")?;
    ui.reveal("local-page", "local-merge")?;
    ui.checkpoint("local-merge-confirmation")?;
    click(&mut ui, "local-merge")?;
    let data = ws.data.join("local-tasks/local-check.json");
    until(&mut ui, "verified local merge", 40, |_| {
        let state = read(&data)?;
        Ok(state["current"]["merged_at"].is_i64().then_some(()))
    })?;
    let state = read(&data)?;
    let approved = state["current"]["delivery"]["candidate"]
        .as_str()
        .context("missing approval")?;
    ensure!(
        git(&ws.root, &["rev-parse", "main"])?.trim() == approved,
        "the merged revision differed from the approval"
    );
    ensure!(
        state["current"]["attempts"]
            .as_array()
            .is_some_and(|runs| runs.len() == 2),
        "retry history was not retained"
    );
    ui.reveal("local-page", "local-ticket-local-check")?;
    ui.checkpoint("local-merged")?;
    if let Some(output) = &args.output_dir {
        fs::write(
            output.join("local-result.json"),
            serde_json::to_vec_pretty(&state)?,
        )?;
    }
    Ok(())
}
