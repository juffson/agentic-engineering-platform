use super::{
    Args, Driver, handoff_fixture,
    https_pilot::{choose, sessions},
    https_queue::until,
};
use aep::workspace::{Workspace, read};
use aep_studio::rolling_gate::{Action, Gate};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    fs,
    io::{self, IsTerminal, Write},
    path::Path,
};

fn click(ui: &mut Driver<'_>, id: &str) -> Result<()> {
    ui.reveal("https-pilot-page", id)?;
    ui.click(id)
}

fn check_failure(root: &Path) -> Result<()> {
    let status: Value = read(&root.join("rolling-state.json")).unwrap_or_default();
    ensure!(
        status["exit_code"].as_i64().is_none_or(|code| code == 0),
        "rolling rehearsal failed at {}. Read {}",
        status["stage"],
        root.join("rolling.log").display(),
    );
    Ok(())
}

fn pause_gate(ui: &mut Driver<'_>, root: &Path, gate: &Gate) -> Result<()> {
    click(ui, "rolling-stop")?;
    until(ui, "the paused Studio rehearsal", 70, |ui| {
        let status = read(&root.join("rolling-state.json")).unwrap_or_default();
        Ok((status["exit_code"] == 4 && !ui.absent("rolling-resume")?).then_some(()))
    })?;
    let paused = fs::read(root.join("gate.json"))?;
    ensure!(
        &serde_json::from_slice::<Gate>(&paused)? == gate,
        "pause changed the pending approval"
    );
    Ok(())
}

fn resume_gate(
    ui: &mut Driver<'_>,
    root: &Path,
    output: &Path,
    gate: &Gate,
    induce_failure: bool,
) -> Result<Gate> {
    if induce_failure {
        let site = read(&root.join("site.json"))?;
        let access = output.join("retry-access").canonicalize()?;
        ensure!(
            site["access"].as_str() == access.to_str(),
            "retry test must use its own access copy"
        );
        let ca = access.join("ca.pem");
        let unavailable = access.join("ca.paused");
        fs::rename(&ca, &unavailable)?;
        let attempt = (|| {
            click(ui, "rolling-resume")?;
            until(ui, "the failed preflight and retry button", 70, |ui| {
                let status = read(&root.join("rolling-state.json")).unwrap_or_default();
                Ok((status["exit_code"] == 1
                    && status["reason"] == "The site access directory needs operator and ca.pem"
                    && ui
                        .element("rolling-resume")
                        .is_ok_and(|button| button.label() == Some("安全重试")))
                .then_some(()))
            })?;
            ui.reveal("https-pilot-page", "rolling-resume")?;
            ui.checkpoint("failed-resume-is-retryable")
        })();
        fs::rename(unavailable, ca)?;
        attempt?;
    }
    click(ui, "rolling-resume")?;
    until(ui, "the original approval after restart", 180, |ui| {
        if ui.absent("rolling-stop")? {
            return Ok(None);
        }
        let current: Gate = serde_json::from_slice(&fs::read(root.join("gate.json"))?)?;
        ensure!(&current == gate, "retry returned to a different approval");
        Ok(Some(current))
    })
}

fn history_id(root: &Path) -> Result<&str> {
    root.file_name()
        .and_then(|id| id.to_str())
        .context("missing history entry ID")
}

fn history_selected(ui: &mut Driver<'_>, root: &Path) -> Result<()> {
    let expected = history_id(root)?;
    until(ui, expected, 20, |ui| {
        Ok(ui
            .element("rolling-history-current")
            .is_ok_and(|item| item.label() == Some(expected))
            .then_some(()))
    })
}

fn choose_history(ui: &mut Driver<'_>, root: &Path) -> Result<()> {
    let id = format!("rolling-history-entry-{}", history_id(root)?);
    until(ui, &id, 20, |ui| Ok((!ui.absent(&id)?).then_some(())))?;
    ui.reveal("https-pilot-page", "rolling-history-list")?;
    choose(ui, "rolling-history-list", &id)?;
    history_selected(ui, root)
}

fn history_scopes(ui: &mut Driver<'_>, site: &str, older: &Path, recent: &Path) -> Result<()> {
    click(ui, &format!("rolling-history-site-{site}"))?;
    for worker in [format!("{site}-worker"), format!("{site}-worker-two")] {
        click(ui, &format!("rolling-history-worker-{worker}"))?;
        ensure!(
            !ui.absent(&format!("rolling-history-entry-{}", history_id(older)?))?
                && !ui.absent(&format!("rolling-history-entry-{}", history_id(recent)?))?,
            "the site and Worker filters lost a matching rehearsal"
        );
    }
    Ok(())
}

fn archived_history(ui: &mut Driver<'_>, root: &Path) -> Result<()> {
    choose_history(ui, root)?;
    ensure!(
        !ui.absent("rolling-archive")?
            && ui.absent("rolling-resume")?
            && !ui.absent("rolling-receipt-5")?
            && ui.absent("rolling-receipt-6")?
            && !ui.absent("rolling-attempt-0")?,
        "the earlier archive has unexpected approval or attempt records",
    );
    Ok(())
}

fn retry_failed_history(ui: &mut Driver<'_>, root: &Path) -> Result<bool> {
    let attempts = || fs::read_dir(root.join("evidence/attempts")).map(|entries| entries.count());
    let before = attempts()?;
    ensure!(
        ui.element("rolling-resume")?.label() == Some("安全重试"),
        "history did not expose the selected failed rehearsal's retry"
    );
    ui.reveal("https-pilot-page", "rolling-resume")?;
    ui.checkpoint("history-selected-failed")?;
    click(ui, "rolling-resume")?;
    let reached_gate = until(
        ui,
        "the retried GitHub CI or original merge approval",
        2_950,
        |ui| {
            let state = read(&root.join("rolling-state.json")).unwrap_or_default();
            if attempts()? > before && state["exit_code"] == 1 {
                return Ok(Some(false));
            }
            Ok((!ui.absent("rolling-stop")?).then_some(true))
        },
    )?;
    if !reached_gate {
        until(ui, "the saved failure reason in Studio", 30, |ui| {
            let states = fs::read_dir(root.join("evidence/attempts"))?.count();
            Ok((!ui.absent("rolling-failure")?
                && !ui.absent(&format!("rolling-attempt-{}", states - 1))?)
            .then_some(()))
        })?;
        ui.reveal("https-pilot-page", "rolling-failure")?;
        ui.checkpoint("history-retry-failure-retained")?;
        println!(
            "The history retry returned to the same failed rehearsal. GitHub CI is still blocked."
        );
    }
    Ok(reached_gate)
}

pub(super) fn run(mut args: Args) -> Result<()> {
    ensure!(
        io::stdin().is_terminal(),
        "the Studio UI verification requires an operator terminal"
    );
    let site = std::env::var("AEP_STUDIO_ROLLING_SITE").context("set AEP_STUDIO_ROLLING_SITE")?;
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize()?;
    let ws = Workspace::open(&source)?;
    let output = args
        .output_dir
        .clone()
        .context("missing output directory")?;
    fs::create_dir_all(&output)?;
    args.screenshots = true;
    let parent = ws.data.join("https-rolling-studio");
    let phase = std::env::var("AEP_STUDIO_ROLLING_RESTART").unwrap_or_default();
    let history_previous =
        std::env::var_os("AEP_STUDIO_ROLLING_HISTORY_PREVIOUS").map(std::path::PathBuf::from);
    if let Some(previous) = &history_previous {
        ensure!(
            previous.parent() == Some(parent.as_path()) && previous.is_dir(),
            "previous run must be an archived Studio rehearsal"
        );
    }
    let runner = aep_studio::Runner {
        binary: Some(handoff_fixture::build_runner()?),
        client_bin: None,
    };
    let mut ui = Driver::open_with_runner(&ws, &args, runner)?;
    ui.click("HTTPS 试点")?;
    let reopened = phase == "reopen";
    let from_history = matches!(phase.as_str(), "history-close" | "history-retry");
    let root = if reopened || from_history {
        let root = std::path::PathBuf::from(fs::read_to_string(output.join("studio-session.txt"))?);
        ensure!(
            root.parent() == Some(parent.as_path()),
            "unexpected saved rehearsal directory"
        );
        if from_history {
            let previous = history_previous
                .as_ref()
                .context("set the earlier archive for the history test")?;
            if phase == "history-close" {
                history_scopes(&mut ui, &site, previous, &root)?;
                ui.reveal("https-pilot-page", "rolling-history-list")?;
                ui.checkpoint("history-filtered-runs")?;
                choose_history(&mut ui, &root)?;
                ensure!(
                    !ui.absent("rolling-resume")? && !ui.absent("rolling-attempt-0")?,
                    "the failed rehearsal is missing its saved attempt"
                );
                archived_history(&mut ui, previous)?;
                ui.reveal("https-pilot-page", "rolling-history-current")?;
                ui.checkpoint("history-archived-first")?;
                return Ok(());
            }
            history_selected(&mut ui, previous)?;
            choose_history(&mut ui, &root)?;
            if !retry_failed_history(&mut ui, &root)? {
                return Ok(());
            }
        } else if let Some(previous) = &history_previous {
            history_selected(&mut ui, previous)?;
            choose_history(&mut ui, &root)?;
            let status = read(&root.join("rolling-state.json"))?;
            ensure!(
                status["exit_code"] == 4 && !ui.absent("rolling-resume")?,
                "the interrupted history entry is not resumable"
            );
            ui.reveal("https-pilot-page", "rolling-resume")?;
            ui.checkpoint("history-selected-interrupted")?;
        }
        root
    } else {
        let before = sessions(&parent);
        ui.click("https-rolling-mode")?;
        let site_id = format!("rolling-site-{site}");
        until(&mut ui, "the deployed site", 35, |ui| {
            Ok((!ui.absent(&site_id)?).then_some(()))
        })?;
        click(&mut ui, &site_id)?;
        click(&mut ui, "rolling-probe")?;
        for worker in [&format!("{site}-worker"), &format!("{site}-worker-two")] {
            let id = format!("rolling-worker-{worker}");
            until(&mut ui, worker, 40, |ui| {
                if !ui.absent("rolling-action-error")? {
                    anyhow::bail!(
                        "{}",
                        ui.element("rolling-action-error")?
                            .label()
                            .unwrap_or_default()
                    );
                }
                Ok((!ui.absent(&id)?).then_some(()))
            })?;
            click(&mut ui, &id)?;
        }
        ui.reveal("https-pilot-page", "rolling-start")?;
        ui.checkpoint("01-rolling-site-and-workers")?;
        ui.click("rolling-start")?;
        let root = until(&mut ui, "a Studio rolling session", 30, |ui| {
            if !ui.absent("rolling-action-error")? {
                anyhow::bail!(
                    "{}",
                    ui.element("rolling-action-error")?
                        .label()
                        .unwrap_or_default()
                );
            }
            Ok(sessions(&parent).difference(&before).next().cloned())
        })?;
        println!("Studio rolling rehearsal: {}", root.display());
        until(&mut ui, "the cutover failure in Studio", 200, |ui| {
            check_failure(&root)?;
            Ok((!ui.absent("rolling-cutover-error")?).then_some(()))
        })?;
        ui.reveal("https-pilot-page", "rolling-cutover-error")?;
        ui.checkpoint("02-studio-cutover-failed")?;
        fs::write(
            output.join("studio-session.txt"),
            root.display().to_string(),
        )?;
        root
    };
    let mut previous = None;
    for (index, expected) in ["publish", "merge", "revoke", "publish", "merge", "revoke"]
        .into_iter()
        .enumerate()
        .skip(usize::from(reopened || phase == "history-retry"))
    {
        let gate: Gate = until(&mut ui, expected, 2_950, |ui| {
            check_failure(&root)?;
            let gate = fs::read(root.join("gate.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Gate>(&bytes).ok())
                .filter(|gate| previous.as_ref() != Some(&gate.id));
            Ok(gate.filter(|_| ui.absent("rolling-gate-scope").is_ok_and(|absent| !absent)))
        })?;
        if phase == "close" && index == 1 {
            let file = fs::File::create(output.join("first-merge-before-close.json"))?;
            serde_json::to_writer_pretty(file, &gate)?;
            ui.reveal("https-pilot-page", "rolling-gate-scope")?;
            ui.checkpoint("close-at-first-merge-approval")?;
            if let Some(previous) = &history_previous {
                pause_gate(&mut ui, &root, &gate)?;
                history_scopes(&mut ui, &site, previous, &root)?;
                ui.reveal("https-pilot-page", "rolling-history-list")?;
                ui.checkpoint("history-filtered-runs")?;
                archived_history(&mut ui, previous)?;
                ui.reveal("https-pilot-page", "rolling-history-current")?;
                ui.checkpoint("history-archived-first")?;
            }
            println!(
                "Closing Studio at the first merge confirmation. The rehearsal stays attached to its saved scope."
            );
            return Ok(());
        }
        let gate = if reopened && index == 1 {
            let before: Gate =
                serde_json::from_slice(&fs::read(output.join("first-merge-before-close.json"))?)?;
            ensure!(
                gate == before,
                "reopened Studio changed the pending merge approval"
            );
            ui.reveal("https-pilot-page", "rolling-gate-scope")?;
            ui.checkpoint("reopened-original-merge-approval")?;
            if history_previous.is_none() {
                pause_gate(&mut ui, &root, &gate)?;
            }
            let gate = resume_gate(&mut ui, &root, &output, &gate, true)?;
            ui.reveal("https-pilot-page", "rolling-gate-scope")?;
            ui.checkpoint("retried-original-merge-approval")?;
            gate
        } else if reopened && index == 5 {
            pause_gate(&mut ui, &root, &gate)?;
            let gate = resume_gate(&mut ui, &root, &output, &gate, false)?;
            ui.reveal("https-pilot-page", "rolling-gate-scope")?;
            ui.checkpoint("resumed-second-worker-retirement")?;
            gate
        } else {
            gate
        };
        ensure!(gate.valid(), "invalid confirmation: {gate:?}");
        let (kind, confirm) = match &gate.action {
            Action::Publish { candidate, .. } => {
                ("publish", format!("publish {}", &candidate[..12]))
            }
            Action::Merge { candidate, pr, .. } => {
                ("merge", format!("merge {pr} {}", &candidate[..12]))
            }
            Action::Revoke { old, .. } => ("revoke", format!("revoke {old}")),
        };
        ensure!(kind == expected, "unexpected confirmation: {gate:?}");
        if kind == "merge" {
            let role = if index == 1 { "peer" } else { "switched" };
            let ci = format!("rolling-ci-{role}-0");
            until(&mut ui, "GitHub checks in Studio", 20, |ui| {
                Ok((!ui.absent(&ci)?).then_some(()))
            })?;
            ui.reveal("https-pilot-page", &ci)?;
            ui.checkpoint(&format!("ci-{role}"))?;
        }
        println!("{} · {} · {:?}", gate.id, gate.worker, gate.action);
        ensure!(
            ui.absent("rolling-confirm")?,
            "Studio skipped acknowledgement"
        );
        ui.reveal("https-pilot-page", "rolling-gate-scope")?;
        ui.checkpoint(&format!("gate-{}-{kind}", index + 1))?;
        print!("Type {confirm} to approve through Studio: ");
        io::stdout().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        ensure!(answer.trim() == confirm, "operator declined Studio action");
        click(&mut ui, "rolling-ack")?;
        click(&mut ui, "rolling-confirm")?;
        previous = Some(gate.id);
    }

    until(&mut ui, "both archives in Studio", 120, |ui| {
        check_failure(&root)?;
        let status = read(&root.join("rolling-state.json")).unwrap_or_default();
        Ok((status["exit_code"] == 0
            && status["stage"] == "completed"
            && !ui.absent("rolling-archive")?)
        .then_some(()))
    })?;
    let result: Value = read(&root.join("evidence/result.json"))?;
    ensure!(
        result["exit_code"] == 0 && result["stage"] == "completed",
        "unverified rehearsal result"
    );
    for role in ["peer", "switched"] {
        ensure!(
            !ui.absent(&format!("rolling-delivery-{role}"))?
                && !ui.absent(&format!("rolling-ci-{role}-0"))?,
            "Studio did not retain the {role} task and CI",
        );
    }
    ui.reveal("https-pilot-page", "rolling-archive")?;
    ui.checkpoint("archive-in-studio")?;
    click(&mut ui, "rolling-shot-0")?;
    ui.reveal("https-pilot-page", "rolling-preview")?;
    ui.checkpoint("screenshot-preview-in-studio")?;
    if reopened && let Some(previous) = &history_previous {
        archived_history(&mut ui, previous)?;
        choose_history(&mut ui, &root)?;
        ensure!(
            !ui.absent("rolling-receipt-5")? && ui.absent("rolling-receipt-6")?,
            "resumed rehearsal did not retain six distinct approvals"
        );
        let mut failed_attempt = None;
        for index in 0..4 {
            let element = format!("rolling-attempt-reason-{index}");
            if ui.element(&element).is_ok_and(|item| {
                item.label() == Some("The site access directory needs operator and ca.pem")
            }) {
                failed_attempt = Some(element);
            }
        }
        let failed_attempt =
            failed_attempt.context("the resumed entry lost its failed retry reason")?;
        ui.reveal("https-pilot-page", &failed_attempt)?;
        ui.checkpoint("history-second-retry-and-receipts")?;
        ui.reveal("https-pilot-page", "rolling-history-list")?;
        ui.checkpoint("history-two-archives")?;
    }
    fs::copy(root.join("evidence.tar.gz"), output.join("evidence.tar.gz"))?;
    fs::write(
        output.join("studio-session.txt"),
        root.display().to_string(),
    )?;
    println!(
        "Verified the two Workers, six Studio approvals and the archive: {}",
        output.display()
    );
    Ok(())
}
