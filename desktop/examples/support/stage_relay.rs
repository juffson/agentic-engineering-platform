use super::{Args, Driver, https_queue::until, project::click};
use aep::{
    project::{Kind, Library, RecordKind, save_json, timeline},
    studio::Snapshot,
    workflow::Template,
    workspace::{Workspace, read, status},
};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::json;
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
    #[serde(default)]
    spec_sha: Option<String>,
    first: Models,
    second: Models,
    decision: String,
    #[serde(default)]
    operator_gates: bool,
}

use aep_studio::stage_lifecycle::Models;

pub(super) fn verify_actual_model_fixture(
    ui: &mut Driver<'_>,
    ws: &Workspace,
    home: &Path,
    runner: aep_studio::Runner,
    relay: &str,
    run: &str,
) -> Result<()> {
    let models = [
        (
            "edit",
            "desktop",
            "account:bigmodel-start-plan",
            "BigModel Start Plan",
        ),
        (
            "plan",
            "app-server",
            "account:bigmodel-individual-coding-plan",
            "BigModel 个人 Coding Plan",
        ),
    ];
    for (mode, transport, provider, _) in models {
        let dir = ws.data.join("runs").join(run).join(format!("zcode-{mode}"));
        fs::create_dir_all(&dir)?;
        save_json(
            &dir,
            "session.json",
            &json!({
                "transport":transport,"providerId":provider,"modelId":"glm","reasoningLevel":"high",
                "session":format!("model-{mode}"),"turn":format!("turn-{mode}"),"workspace":ws.root
            }),
        )?;
    }
    let mut library = Library::open(home)?;
    let item = library.change(home, |library| {
        let project = library.project_mut()?;
        project.add_work("接力实际模型", Kind::Feature, Template::Software)?;
        let work = project.selected_mut()?;
        work.repositories = vec![ws.root.clone()];
        let item = work.id.clone();
        project.link_record(&item, &ws.root, RecordKind::Relay, relay)?;
        Ok(item)
    })?;
    *ui = Driver::open_with_runner(ws, ui.args, runner)?;
    ui.click("两轮接力")?;
    press(ui, "relay-list", relay)?;
    for (mode, _, _, plan) in models {
        let id = format!("relay-second-{mode}-receipt");
        ensure!(
            ui.element(&id)?.label()
                == Some(format!("{plan} / glm / high · 会话 model-{mode}").as_str()),
            "wrong {mode} receipt on the relay card"
        );
    }
    ui.reveal("relay-detail", "relay-second-edit-receipt")?;
    ui.checkpoint("zcode-relay-actual-models")?;
    ui.click("back-to-project")?;
    super::stage_lifecycle::open_item(ui, &item)?;
    click(ui, "work-timeline-tab")?;
    let project = library.project().context("project")?;
    let timeline = timeline::load(home, &project.id, project.selected().context("work")?)?;
    let index = timeline
        .rounds
        .iter()
        .position(|round| round.run == run)
        .context("relay round")?;
    let detail = format!("timeline-detail-{index}");
    until(ui, &detail, 30, |ui| {
        Ok((!ui.absent(&detail)?).then_some(()))
    })?;
    for (agent, (_, _, _, plan)) in models.into_iter().enumerate() {
        let id = format!("timeline-model-{index}-{agent}");
        ensure!(
            ui.element(&id)?.label() == Some(format!("{plan} / glm / high").as_str()),
            "wrong actual model on timeline agent {agent}"
        );
    }
    ui.reveal("project-page", &detail)?;
    ui.checkpoint("zcode-relay-timeline")?;
    Ok(())
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
    super::stage_lifecycle::open_item(ui, &plan.work)?;
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
            fs::create_dir_all(&folder)?;
            let baseline = out.join("before-dispatch.json");
            let existing: Vec<PathBuf> = if baseline.try_exists()? {
                serde_json::from_slice(&fs::read(&baseline)?)?
            } else {
                let entries = fs::read_dir(&folder)?
                    .map(|e| e.map(|e| e.path()))
                    .collect::<std::io::Result<Vec<_>>>()?;
                aep::project::save_json(out, "before-dispatch.json", &entries)?;
                entries
            };
            let dispatched = || -> Result<Option<String>> {
                let mut ids = vec![];
                for item in fs::read_dir(&folder)? {
                    let dir = item?.path();
                    if !existing.contains(&dir)
                        && let Ok(s) = read(&dir.join("relay.json"))
                        && s["stage_plan"]["work"] == plan.work
                        && s["stage_plan"]["stage"] == plan.stage - 1
                        && s["task"]["name"] == plan.task
                    {
                        ids.push(s["id"].as_str().context("relay id")?.to_owned());
                    }
                }
                ensure!(ids.len() <= 1, "同一事项出现多个新接力，请核对 relay-id");
                Ok(ids.pop())
            };
            let id = if let Some(id) = dispatched()? {
                id
            } else {
                press(ui, "relay-list", &format!("relay-task-{}", plan.task))?;
                if let Some(sha) = &plan.spec_sha {
                    ensure!(
                        ui.element("relay-selected-spec")?.label() == Some(sha.as_str()),
                        "任务 Spec 已变化，请重新选择"
                    );
                }
                models(ui, &plan.first)?;
                press(ui, "relay-detail", "stage-agents-second")?;
                models(ui, &plan.second)?;
                ui.checkpoint("stage-models")?;
                press(ui, "relay-detail", "start-relay")?;
                until(ui, "stage relay", 45, |_| dispatched())?
            };
            fs::write(&handle, &id)?;
            id
        };
        let saved = folder.join(&id).join("relay.json");
        let state = read(&saved)?;
        ensure!(
            state["stage_plan"]["work"] == plan.work
                && state["stage_plan"]["stage"] == plan.stage - 1
                && state["task"]["name"] == plan.task
                && plan
                    .spec_sha
                    .as_ref()
                    .is_none_or(|sha| state["task"]["sha"] == *sha),
            "saved relay belongs to a different task"
        );
        // Reopen from the durable project catalog, even on the first invocation.
        *ui = Driver::open(&ws, ui.args)?;
        enter(ui, &plan)?;
        until(ui, "saved stage record", 30, |ui| {
            Ok((!ui.absent(&id)?).then_some(()))
        })?;
        press(ui, "relay-list", &id)?;
        if let Some(retry) = &args.retry_attempt
            && let Some(last) = state["attempts"].as_array().and_then(|a| a.last())
            && last["id"] == *retry
        {
            let control = if last["round"] == "First" {
                "retry-relay-first".to_owned()
            } else {
                format!(
                    "relay-continue-{}",
                    last["decision"].as_i64().context("已确认的第二轮决策")?
                )
            };
            until(ui, "原接力重试入口", 30, |ui| {
                Ok((!ui.absent(&control)?).then_some(()))
            })?;
            press(ui, "relay-detail", &control)?;
            until(ui, "新的接力尝试", 30, |_| {
                Ok(read(&saved)?["attempts"]
                    .as_array()
                    .and_then(|a| a.last())
                    .is_some_and(|a| a["id"] != *retry)
                    .then_some(()))
            })?;
        }
        // The standalone relay driver also permits correcting a bad first-round model.
        if !args.restore_work_window
            && args.retry_attempt.is_none()
            && let Some(last) = state["attempts"].as_array().and_then(|a| a.last())
            && last["round"] == "First"
            && let Some(ev) = last["id"].as_str().and_then(|id| status(&ws.data, id).ok())
            && ev["status"] == "failed"
            && ev["request"]["codex_model"] != plan.first.implementation_model
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
                let run_id = attempt["id"].as_str().context("接力执行 ID")?;
                let run = status(&ws.data, run_id).unwrap_or_default();
                if (run["status"].is_null() || run["status"] == "unstarted")
                    && let Ok(lock) = fs::File::open(folder.join(&id).join("owner.lock"))
                    && lock.try_lock_shared().is_ok()
                {
                    let msg =
                        aep::workflow::tail(&folder.join(&id).join(format!("{run_id}.log")), 2500);
                    return super::stage_lifecycle::failed_attempt(
                        out,
                        name,
                        run_id,
                        &format!("执行未启动 {msg}"),
                    );
                }
                match run["status"].as_str() {
                    Some("failed" | "cancelled" | "blocked" | "needs_reconciliation") => {
                        let reason = run["error"]
                            .as_str()
                            .or_else(|| run["status"].as_str())
                            .unwrap_or("接力失败");
                        super::stage_lifecycle::failed_attempt(out, name, run_id, reason)
                    }
                    Some("awaiting_human_review") => Ok(Some(run)),
                    _ => Ok(None),
                }
            })?;
            let req = &evidence["request"];
            selection
                .verify_receipt(&evidence)
                .with_context(|| format!("{name} 回执"))?;
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
                if plan.operator_gates {
                    ui.checkpoint("stage-question")?;
                    super::stage_lifecycle::wait_for_operator(
                        out,
                        "decision",
                        &format!(
                            "在事项 {} 的阶段接力记录 {} 中将问题 #{q} 修订为决策、点击用此决策发起第二轮",
                            plan.work, id
                        ),
                        || {
                            Ok(read(&saved)?["attempts"]
                                .as_array()
                                .context("attempts")?
                                .iter()
                                .any(|a| a["round"] == "Second"))
                        },
                    )?;
                    *ui = Driver::open(&ws, ui.args)?;
                    enter(ui, &plan)?;
                    press(ui, "relay-list", &id)?;
                    continue;
                }
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
                let snapshot = Snapshot::load(&ws);
                let first = read(&out.join("first-run.json"))?;
                let revision = snapshot
                    .notes
                    .iter()
                    .find(|n| n["id"] == decision && n["kind"] == "decision")
                    .context("决策版本")?;
                let question = snapshot
                    .notes
                    .iter()
                    .find(|n| {
                        n["id"] == revision["replaces"]
                            && n["kind"] == "open_question"
                            && n["run_id"] == first["run_id"]
                    })
                    .context("决策未修订本轮的问题")?;
                ensure!(
                    plan.operator_gates || revision["body"] == plan.decision,
                    "修订正文与演练计划不一致"
                );
                aep::project::save_json(
                    out,
                    "decision.json",
                    &serde_json::json!({"question":question,"revision":revision}),
                )?;
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
                        agent.reads.iter().any(|n| n.id == decision
                            && (plan.operator_gates || n.body == plan.decision)),
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
