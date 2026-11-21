use super::{Args, Clients, Driver, handoff_fixture, https_queue::until};
use aep::workspace::{git, read};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
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
    let spec = "Implement milliseconds in normalize.rs with a widened result.";
    let manifest: Value = serde_json::from_str(include_str!("handoff/task.json"))?;
    click(&mut ui, "local-ticket")?;
    ui.input("local-check")?;
    available(&mut ui, "local-target-main")?;
    click(&mut ui, "local-target-main")?;
    click(&mut ui, "local-new-spec")?;
    for (id, value) in [
        ("task-author-name", "studio-local"),
        ("task-author-title", "Convert seconds to milliseconds"),
        ("task-author-spec", spec),
        ("task-author-paths", "normalize.rs"),
        ("task-author-check", &manifest["check"].to_string()),
    ] {
        click(&mut ui, id)?;
        ui.input(value)?;
    }
    ui.checkpoint("local-spec-authoring")?;
    git(
        &ws.root,
        &[
            "commit",
            "--allow-empty",
            "-qm",
            "Advance target during editing",
        ],
    )?;
    let baseline = git(&ws.root, &["rev-parse", "main"])?;
    click(&mut ui, "local-save-spec")?;
    available(&mut ui, "local-command-error")?;
    ensure!(
        ui.element("local-command-error")?
            .label()
            .is_some_and(|text| text.contains("target branch moved")),
        "Studio did not explain that the editor's target had moved"
    );
    available(&mut ui, "local-update-baseline")?;
    click(&mut ui, "local-update-baseline")?;
    click(&mut ui, "local-save-spec")?;
    available(&mut ui, "local-approve-plan")?;
    click(&mut ui, "local-approve-plan")?;
    available(&mut ui, "local-run")?;
    click(&mut ui, "local-edit-spec")?;
    click(&mut ui, "task-author-spec")?;
    ui.replace_focused(&format!("{spec} Check the maximum u32 input."))?;
    click(&mut ui, "local-author-model")?;
    ui.input("fixture-model")?;
    click(&mut ui, "local-save-spec")?;
    available(&mut ui, "local-approve-plan")?;
    let data = ws.data.join("local-tasks/local-check.json");
    let revised = read(&data)?;
    ensure!(
        revised["current"]["version"] == 2
            && revised["current"]["codex_model"] == "fixture-model"
            && revised["current"]["plan_approval"].is_null()
            && revised["history"][0]["plan_approval"].is_object()
            && git(&ws.root, &["rev-parse", "main"])? == baseline,
        "Studio did not retain the initial plan and require a fresh approval"
    );
    ui.reveal("local-page", "local-approve-plan")?;
    ui.checkpoint("local-plan-approval")?;
    click(&mut ui, "local-approve-plan")?;
    available(&mut ui, "local-run")?;
    click(&mut ui, "local-run")?;
    available(&mut ui, "local-run-reason")?;
    ui.reveal("local-page", "local-run-reason")?;
    ui.checkpoint("local-check-failed")?;
    let failed = read(&data)?;
    let first_run = failed["current"]["attempts"][0]
        .as_str()
        .context("missing first Run")?;

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
    click(&mut ui, "local-version-1")?;
    available(&mut ui, "local-evidence-text")?;
    let evidence = ui.element("local-evidence-text")?;
    ensure!(
        evidence
            .label()
            .is_some_and(|text| text.contains(spec) && !text.contains("maximum u32 input"))
            && ui.absent("local-approve-delivery")?,
        "Studio did not show the original Spec as read-only evidence",
    );
    ui.reveal("local-page", "local-evidence-text")?;
    ui.checkpoint("local-prior-spec")?;
    click(&mut ui, "local-current-version")?;
    available(&mut ui, "local-approve-delivery")?;
    click(&mut ui, &format!("local-run-evidence-{first_run}"))?;
    available(&mut ui, "local-run-reason")?;
    ensure!(
        ui.absent("local-approve-delivery")?,
        "an earlier Run exposed delivery approval"
    );
    ui.reveal("local-page", "local-run-reason")?;
    ui.checkpoint("local-prior-run")?;
    click(&mut ui, "local-current-version")?;
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
    available(&mut ui, "local-merge-receipt")?;
    ui.reveal("local-page", "local-merge-receipt")?;
    ui.checkpoint("local-merged")?;
    if let Some(output) = &args.output_dir {
        handoff_fixture::export_runs(
            &ws,
            &output.join("local-runs"),
            state["current"]["attempts"]
                .as_array()
                .context("missing Runs")?
                .iter()
                .filter_map(Value::as_str),
        )?;
        fs::write(
            output.join("local-result.json"),
            serde_json::to_vec_pretty(&state)?,
        )?;
    }
    Ok(())
}
