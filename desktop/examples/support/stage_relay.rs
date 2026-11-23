use super::{Args, Driver, https_queue::until, project::click};
use aep::{
    studio::Snapshot,
    workspace::{Workspace, read, status},
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
    task: String,
    first: Models,
    second: Models,
    decision: String,
}

#[derive(Deserialize)]
struct Models {
    implementer: String,
    implementation_model: String,
    reviewer: String,
    review_model: String,
}

fn press(ui: &mut Driver<'_>, container: &str, id: &str) -> Result<()> {
    if !ui.element(id)?.visible() {
        ui.reveal(container, id)?;
    }
    ui.click(id)
}

fn models(ui: &mut Driver<'_>, pick: &Models) -> Result<()> {
    for (role, client) in [
        ("implementer", &pick.implementer),
        ("reviewer", &pick.reviewer),
    ] {
        press(ui, "relay-detail", &format!("relay-{role}-{client}"))?;
    }
    let implementation = if pick.implementer == "zcode" {
        ("relay-zcode-model", "ZCode 模型")
    } else {
        ("relay-model", "Codex 模型")
    };
    for ((id, label), value) in [
        (implementation, &pick.implementation_model),
        (("relay-reviewer-model", "审查模型"), &pick.review_model),
    ] {
        if !ui.by_label(label)?.visible() {
            ui.reveal("relay-detail", id)?;
        }
        let field = ui.by_label(label)?;
        ui.click_target(field)?;
        ui.replace_focused(value)?;
        ensure!(
            ui.by_label(label)?.value() == Some(value),
            "model selection failed: {label}"
        );
    }
    Ok(())
}

fn enter(ui: &mut Driver<'_>, plan: &Plan) -> Result<()> {
    until(ui, "project", 30, |ui| {
        Ok((!ui.absent("project-overview")?).then_some(()))
    })?;
    ui.click("project-overview")?;
    click(ui, &format!("project-item-{}", plan.work))?;
    click(
        ui,
        &format!(
            "project-stage-{}",
            plan.stage.checked_sub(1).context("stage starts at 1")?
        ),
    )?;
    click(ui, "stage-relay")
}

pub(super) fn run(args: &Args, path: &Path) -> Result<()> {
    let plan: Plan = serde_json::from_slice(&fs::read(path)?)?;
    let ws = Workspace::open(&plan.repository)?;
    let out = args.output_dir.as_ref().context("output directory")?;
    super::inspect(args, &ws, |ui| {
        enter(ui, &plan)?;
        let handle = out.join("relay-id");
        let folder = ws.data.join("relays");
        let id = if handle.is_file() {
            fs::read_to_string(&handle)?
        } else {
            press(ui, "relay-list", &format!("relay-task-{}", plan.task))?;
            models(ui, &plan.first)?;
            press(ui, "relay-detail", "stage-agents-second")?;
            models(ui, &plan.second)?;
            ui.checkpoint("stage-models")?;
            fs::create_dir_all(&folder)?;
            let existing = fs::read_dir(&folder)?
                .collect::<std::io::Result<Vec<_>>>()?
                .into_iter()
                .map(|e| e.path())
                .collect::<Vec<_>>();
            press(ui, "relay-detail", "start-relay")?;
            let id = until(ui, "stage relay", 45, |_| {
                for item in fs::read_dir(&folder)? {
                    let dir = item?.path();
                    if !existing.contains(&dir)
                        && let Ok(s) = read(&dir.join("relay.json"))
                        && s["stage_plan"]["work"] == plan.work
                        && s["task"]["name"] == plan.task
                    {
                        return Ok(s["id"].as_str().map(str::to_owned));
                    }
                }
                Ok(None)
            })?;
            fs::write(&handle, &id)?;
            id
        };
        let saved = folder.join(&id).join("relay.json");
        let state = read(&saved)?;
        ensure!(
            state["stage_plan"]["work"] == plan.work
                && state["stage_plan"]["stage"] == plan.stage - 1
                && state["task"]["name"] == plan.task,
            "saved relay belongs to a different task"
        );
        // Reopen from the durable project catalog, even on the first invocation.
        *ui = Driver::open(&ws, ui.args)?;
        enter(ui, &plan)?;
        until(ui, "saved stage record", 30, |ui| {
            Ok((!ui.absent(&id)?).then_some(()))
        })?;
        press(ui, "relay-list", &id)?;
        if let Some(attempt) = state["attempts"].as_array().and_then(|a| a.last())
            && attempt["round"] == "First"
            && let Some(failed) = attempt["id"]
                .as_str()
                .and_then(|id| status(&ws.data, id).ok())
            && failed["status"] == "failed"
            && failed["request"]["codex_model"] != plan.first.implementation_model
        {
            models(ui, &plan.first)?;
            press(ui, "relay-detail", "retry-relay-first")?;
        }
        for (i, selection) in [&plan.first, &plan.second].into_iter().enumerate() {
            let name = if i == 0 { "first" } else { "second" };
            let evidence = until(ui, name, 900, |_| {
                let state = read(&saved)?;
                let Some(attempt) = state["attempts"]
                    .as_array()
                    .context("attempts")?
                    .iter()
                    .rev()
                    .find(|a| {
                        a["round"]
                            .as_str()
                            .is_some_and(|r| r.eq_ignore_ascii_case(name))
                    })
                else {
                    return Ok(None);
                };
                ensure!(
                    attempt["diagnostics"].is_null(),
                    "{name}: {}",
                    attempt["diagnostics"]
                );
                let Some(run) = attempt["id"]
                    .as_str()
                    .and_then(|id| status(&ws.data, id).ok())
                else {
                    return Ok(None);
                };
                match run["status"].as_str() {
                    Some("failed" | "cancelled" | "blocked") => {
                        anyhow::bail!("{name}: {}", run["error"])
                    }
                    Some("awaiting_human_review") => Ok(Some(run)),
                    _ => Ok(None),
                }
            })?;
            let req = &evidence["request"];
            let model = if selection.implementer == "codex" {
                "codex_model"
            } else {
                "zcode_model"
            };
            ensure!(
                req[model] == selection.implementation_model
                    && req["reviewer_model"] == selection.review_model
                    && evidence["check_exit_code"] == 0
                    && evidence["review_status"] == "findings_recorded",
                "{name}: model or acceptance mismatch"
            );
            fs::write(
                out.join(format!("{name}-run.json")),
                serde_json::to_vec_pretty(&evidence)?,
            )?;
            if !out.join(format!("stage-{name}.json")).exists() {
                ui.checkpoint(&format!("stage-{name}"))?;
            }
            if i == 0 {
                let state = read(&saved)?;
                if state["attempts"]
                    .as_array()
                    .context("saved attempts")?
                    .iter()
                    .any(|a| a["round"] == "Second")
                {
                    continue;
                }
                let first = state["attempts"]
                    .as_array()
                    .context("saved attempts")?
                    .iter()
                    .rev()
                    .find(|a| a["round"] == "First")
                    .context("first run")?;
                let snapshot = Snapshot::load(&ws);
                let question = snapshot
                    .notes
                    .iter()
                    .find(|n| n["run_id"] == first["id"] && n["kind"] == "open_question")
                    .context("task did not leave the requested question")?;
                let q = question["id"].as_i64().context("question id")?;
                if question["successor"].is_null() {
                    let resolve = format!("relay-resolve-{q}");
                    until(ui, &resolve, 30, |ui| {
                        Ok((!ui.absent(&resolve)?).then_some(()))
                    })?;
                    press(ui, "relay-detail", &resolve)?;
                    ui.replace_focused(&plan.decision)?;
                    ui.click("save-note")?;
                }
                let decision = until(ui, "revised decision", 30, |_| {
                    Ok(Snapshot::load(&ws)
                        .notes
                        .iter()
                        .find(|n| n["replaces"] == q && n["body"] == plan.decision)
                        .and_then(|n| n["id"].as_i64()))
                })?;
                let next = format!("relay-continue-{decision}");
                until(ui, &next, 30, |ui| Ok((!ui.absent(&next)?).then_some(())))?;
                if state["stage_plan"]["second"]["model"] != plan.second.implementation_model
                    || state["stage_plan"]["second"]["reviewer_model"] != plan.second.review_model
                {
                    models(ui, &plan.second)?;
                }
                ui.reveal("relay-detail", &next)?;
                ui.checkpoint("stage-decision")?;
                ui.click(&next)?;
            } else {
                let decision = req["require_memory"]
                    .as_i64()
                    .context("selected decision")?;
                for role in ["implementer", "reviewer"] {
                    ensure!(
                        evidence["memory_inputs"][role]
                            .as_array()
                            .context("memory inputs")?
                            .contains(&decision.into()),
                        "{role} missed the decision"
                    );
                }
            }
        }
        ui.click("back-to-project")?;
        click(ui, "work-timeline-tab")?;
        let catalog = aep::project::directory::DirectoryProject::resolve(
            &ws,
            &aep::project::catalog_dir().context("project catalog")?,
            None,
            aep::project::directory::DirectoryAction::Restore,
        )?;
        let project = catalog
            .library
            .projects
            .iter()
            .find(|p| state["stage_plan"]["project"] == p.id)
            .context("project")?;
        let work = project
            .work
            .iter()
            .find(|w| w.id == plan.work)
            .context("item")?;
        let timeline = aep::project::timeline::load(&catalog.home, &project.id, work)?;
        ensure!(
            timeline.errors.is_empty(),
            "timeline: {:?}",
            timeline.errors
        );
        fs::write(
            out.join("timeline.json"),
            serde_json::to_vec_pretty(&timeline)?,
        )?;
        for (i, pick) in [&plan.first, &plan.second].into_iter().enumerate() {
            let (index, round) = timeline
                .rounds
                .iter()
                .enumerate()
                .rev()
                .find(|(_, r)| r.record == id && r.round == Some(1 + i as u32))
                .context("unlinked round")?;
            ensure!(
                round.stage == Some(plan.stage - 1) && round.agents.len() == 2,
                "round stage or roles missing"
            );
            for (agent, client, model) in [
                (
                    &round.agents[0],
                    &pick.implementer,
                    &pick.implementation_model,
                ),
                (&round.agents[1], &pick.reviewer, &pick.review_model),
            ] {
                ensure!(
                    agent.stage == round.stage
                        && agent.client == *client
                        && !agent.handoff.is_empty()
                        && !agent.writes.is_empty(),
                    "agent ownership or handoff mismatch"
                );
                if let Some(dir) = &agent.desktop_dir {
                    let receipt = read(&dir.join("session.json"))?;
                    ensure!(
                        agent.session.as_deref() == receipt["session"].as_str()
                            && format!(
                                "{}/{}",
                                receipt["providerId"].as_str().context("provider")?,
                                receipt["modelId"].as_str().context("model")?
                            ) == *model,
                        "desktop model or session mismatch"
                    );
                }
                if let Some(decision) = round.decision {
                    ensure!(
                        agent
                            .reads
                            .iter()
                            .any(|n| n.id == decision && n.body == plan.decision),
                        "wrong revision in timeline"
                    );
                }
            }
            let item = format!("timeline-detail-{index}");
            until(ui, &item, 30, |ui| Ok((!ui.absent(&item)?).then_some(())))?;
            click(ui, &item)?;
            ui.reveal("project-page", &item)?;
            ui.checkpoint(&format!("stage-timeline-round-{}", i + 1))?;
            if let Some(decision) = round.decision {
                click(ui, &format!("timeline-evidence-{index}"))?;
                for role in ["implementer", "reviewer"] {
                    ensure!(
                        !ui.absent(&format!("memory_inputs-{role}-{decision}"))?,
                        "evidence opened the wrong run"
                    );
                }
                ui.checkpoint("stage-run-evidence")?;
            }
        }
        Ok(())
    })
}
