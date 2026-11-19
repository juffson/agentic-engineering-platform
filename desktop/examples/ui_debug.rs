//! Run Studio's real event handlers in an offscreen GPUI window.
#[path = "support/handoff.rs"]
mod handoff_fixture;

use aep::{
    memory::{Key, Kind, Memory},
    studio::{Artifact, Snapshot, preview},
    workspace::{Workspace, git, status},
};
use anyhow::{Context, Result, ensure};
use clap::{Parser, ValueEnum};
use gpui_kit::{
    AnyWindowHandle, App, HeadlessAppContext, SharedString, Window, WindowBounds, WindowOptions,
    assets::Assets,
    px, size,
    test::{ElementSnapshot, TestEventExt, TestQueryExt, TestWindowExt},
};
use handoff_fixture::Clients;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Copy, ValueEnum)]
enum Scenario {
    History,
    Conflict,
    Handoff,
    StudioRelay,
}

#[derive(Parser)]
#[command(
    about = "Exercise AEP Studio in a temporary Git workspace, using an offscreen GPUI window"
)]
struct Args {
    /// Run just one scenario (default: all)
    #[arg(long, value_enum)]
    scenario: Option<Scenario>,
    /// Replay a JSON array of UI steps in a fresh fixture
    #[arg(long, conflicts_with = "scenario")]
    script: Option<PathBuf>,
    /// Verify a completed handoff in an existing Git repository
    #[arg(long, requires = "run_id", conflicts_with_all = ["scenario", "script"])]
    repo: Option<PathBuf>,
    /// Run to verify in the supplied Git repository
    #[arg(long, requires = "repo")]
    run_id: Option<String>,
    /// Write JSON control snapshots at each checkpoint
    #[arg(long)]
    output_dir: Option<PathBuf>,
    /// Also save PNGs from the platform's offscreen renderer
    #[arg(long, requires = "output_dir")]
    screenshots: bool,
    /// Run two rounds with real Codex and Claude and capture evidence and PNGs
    #[arg(long, requires = "output_dir", conflicts_with_all = ["scenario", "script", "repo"])]
    live_relay: bool,
    /// Override the Codex model for both rounds of the live relay
    #[arg(long, requires = "live_relay")]
    codex_model: Option<String>,
}

struct Driver<'a> {
    cx: HeadlessAppContext,
    window: AnyWindowHandle,
    args: &'a Args,
}

impl<'a> Driver<'a> {
    fn open(ws: &Workspace, args: &'a Args) -> Result<Self> {
        Self::open_with_runner(ws, args, aep_studio::Runner::default())
    }

    fn open_with_runner(
        ws: &Workspace,
        args: &'a Args,
        runner: aep_studio::Runner,
    ) -> Result<Self> {
        let text = gpui_kit::platform::current_platform(true).text_system();
        let mut cx = if args.screenshots {
            HeadlessAppContext::with_platform(
                text,
                Arc::new(Assets),
                gpui_kit::platform::current_headless_renderer,
            )
        } else {
            HeadlessAppContext::with_asset_source(text, Arc::new(Assets))
        };
        cx.update(aep_studio::init);
        let window = cx.update(|cx| {
            aep_studio::open_studio_with_runner(
                Some(ws.clone()),
                runner,
                WindowOptions {
                    show: false,
                    focus: false,
                    window_bounds: Some(WindowBounds::centered(size(px(1120.), px(730.)), cx)),
                    ..Default::default()
                },
                cx,
            )
        })?;
        let mut driver = Self { cx, window, args };
        driver.settle()?;
        Ok(driver)
    }
    fn frame<T>(&mut self, f: impl FnOnce(&mut Window, &mut App) -> T) -> Result<T> {
        self.cx.update_window(self.window, |_, w, cx| f(w, cx))
    }
    fn settle(&mut self) -> Result<()> {
        self.cx.run_until_parked();
        self.frame(|w, cx| w.render_frame(cx))
    }
    fn click(&mut self, id: &str) -> Result<()> {
        let target = self.element(id)?;
        self.click_target(target)
    }
    fn click_target(&mut self, target: ElementSnapshot) -> Result<()> {
        ensure!(
            target.visible(),
            "control is offscreen: {:?}",
            target.path()
        );
        let center = target.bounds().center();
        self.frame(|w, cx| {
            w.pointer_move(center, None, cx);
            w.pointer_down(center, gpui_kit::MouseButton::Left, cx);
            w.pointer_up(center, gpui_kit::MouseButton::Left, cx);
        })?;
        self.settle()
    }
    fn scroll(&mut self, id: &str, dy: f32) -> Result<()> {
        ensure!(
            self.element(id)?.visible(),
            "scroll target {id:?} is offscreen"
        );
        let delta = gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(dy)));
        self.frame(|w, cx| w.scroll(SharedString::from(id.to_owned()), delta, cx))?;
        self.settle()
    }
    fn reveal(&mut self, container: &str, target: &str) -> Result<()> {
        let top = self.element(container)?.bounds().origin.y;
        let target = self.element(target)?.bounds().origin.y;
        self.scroll(container, f32::from(top + px(120.) - target))
    }
    fn input(&mut self, text: &str) -> Result<()> {
        self.frame(|w, cx| w.input(text, cx))?;
        self.settle()
    }
    fn replace_focused(&mut self, text: &str) -> Result<()> {
        self.frame(|w, cx| w.press("cmd-a", cx))?;
        self.input(text)
    }
    fn element(&mut self, id: &str) -> Result<ElementSnapshot> {
        let matches = self.frame(|w, _| w.find_all(SharedString::from(id.to_owned())))?;
        unique(matches, id)
    }
    fn by_label(&mut self, label: &str) -> Result<ElementSnapshot> {
        let matches = self.frame(|w, _| w.find_all_by_label(label))?;
        unique(matches, label)
    }
    fn absent(&mut self, id: &str) -> Result<bool> {
        self.frame(|w, _| w.find_all(SharedString::from(id.to_owned())).is_empty())
    }
    fn checkpoint(&mut self, name: &str) -> Result<()> {
        println!("  {name}");
        let Some(dir) = &self.args.output_dir else {
            return Ok(());
        };
        let elements = self.frame(|w, _| w.elements())?;
        let items: Vec<_> = elements
            .iter()
            .map(|element| {
                let bounds = element.bounds();
                let path: Vec<_> = element.path().iter().map(ToString::to_string).collect();
                json!({
                    "id": path.last(),
                    "path": path,
                    "role": element.role().map(|role| format!("{role:?}")),
                    "label": element.label(),
                    "value": element.value(),
                    "visible": element.visible(),
                    "bounds": [f32::from(bounds.origin.x), f32::from(bounds.origin.y),
                               f32::from(bounds.size.width), f32::from(bounds.size.height)]
                })
            })
            .collect();
        fs::write(
            dir.join(format!("{name}.json")),
            serde_json::to_vec_pretty(&items)?,
        )?;
        if self.args.screenshots {
            self.cx
                .capture_screenshot(self.window)?
                .save(dir.join(format!("{name}.png")))?;
        }
        Ok(())
    }
}

fn unique(mut matches: Vec<ElementSnapshot>, query: &str) -> Result<ElementSnapshot> {
    ensure!(
        matches.len() == 1,
        "expected one control matching {query:?}, found {}",
        matches.len()
    );
    Ok(matches.remove(0))
}

fn fixture(path: &Path) -> Result<(Workspace, String)> {
    git(path, &["init", "--template=", "-q"])?;
    git(
        path,
        &[
            "-c",
            "user.name=AEP UI Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "--allow-empty",
            "-qm",
            "fixture",
        ],
    )?;
    let ws = Workspace::open(path)?;
    let sha = git(path, &["rev-parse", "HEAD"])?.trim().to_owned();
    let run = ws.data.join("runs/sample-run");
    fs::create_dir_all(&run)?;
    fs::write(
        run.join("evidence.json"),
        serde_json::to_vec(
            &json!({"run_id":"sample-run","task":"sample","spec_sha":sha,
         "candidate_sha":sha,"status":"awaiting_human_review","started_at":"2026-01-01T00:00:00Z"}),
        )?,
    )?;
    Ok((ws, sha))
}

fn history(ui: &mut Driver, ws: &Workspace, sha: &str) -> Result<()> {
    ui.click("compose-run-note")?;
    ensure!(
        ui.by_label("任务")?.value() == Some("sample"),
        "run task was not prefilled"
    );
    ensure!(
        ui.by_label("Spec 提交")?.value() == Some(sha),
        "run spec was not prefilled"
    );
    ui.click("save-note")?;
    ensure!(
        ui.element("save-error")?
            .label()
            .is_some_and(|s| s.contains("1,000")),
        "missing blank-body validation"
    );
    ui.checkpoint("history-validation")?;
    ui.click("未决问题")?;
    // The editor focuses its body on entry. Click the textbox again after button interactions.
    focus_body(ui)?;
    ui.input("Which unit?")?;
    ui.click("save-note")?;
    let db = Memory::open(&ws.data)?;
    let key = Key::new("sample", sha, sha);
    let notes = db.context(&key, None)?;
    ensure!(
        notes.len() == 1
            && notes[0]["scope"] == "task"
            && notes[0]["kind"] == "open_question"
            && notes[0]["body"] == "Which unit?",
        "unexpected saved question: {notes:?}"
    );
    let original = notes[0]["id"].as_i64().context("question ID")?;
    ui.click("revise-note")?;
    ensure!(
        ui.by_label("记忆正文")?.value() == Some("Which unit?"),
        "old text missing from editor"
    );
    ui.click("决策")?;
    focus_body(ui)?;
    ui.replace_focused("Use milliseconds.")?;
    ui.click("save-note")?;
    let notes = db.context(&key, Some("later-run"))?;
    ensure!(
        notes.len() == 1
            && notes[0]["replaces"] == original
            && notes[0]["kind"] == "decision"
            && notes[0]["body"] == "Use milliseconds.",
        "unexpected revision: {notes:?}"
    );
    ensure!(
        db.get(original)?["body"] == "Which unit?",
        "original was overwritten"
    );
    ensure!(
        ui.element("note-body")?.label() == Some("Use milliseconds."),
        "current detail displays the wrong revision"
    );
    ui.checkpoint("history-current")?;
    ui.click(&format!("replaces-{original}"))?;
    ensure!(ui.absent("revise-note")?, "historical note is editable");
    let latest = notes[0]["id"].as_i64().context("revision ID")?;
    ensure!(
        ui.element(&format!("successor-{latest}"))?.visible(),
        "historical note has no successor link"
    );
    ensure!(
        ui.element("note-body")?.label() == Some("Which unit?"),
        "historical detail displays the wrong revision"
    );
    ui.checkpoint("history-original")
}

fn focus_body(ui: &mut Driver) -> Result<()> {
    let body = ui.by_label("记忆正文")?;
    ui.click_target(body)
}

fn conflict(ui: &mut Driver, ws: &Workspace) -> Result<()> {
    ui.click("共享记忆")?;
    ui.click("new-note")?;
    ui.input("Shared convention.")?;
    ui.click("save-note")?;
    let mut db = Memory::open(&ws.data)?;
    let key = Key::new("", "", "");
    let current = db.context(&key, None)?;
    ensure!(
        current.len() == 1 && current[0]["body"] == "Shared convention.",
        "project note was not saved"
    );
    let id = current[0]["id"].as_i64().context("project note ID")?;
    ui.click("revise-note")?;
    ui.replace_focused("Unsaved local proposal.")?;
    let other = db.revise(id, Kind::Decision, "Other editor won.")?;
    ui.click("save-note")?;
    ensure!(
        ui.element("save-error")?
            .label()
            .is_some_and(|s| s.contains("已有新版本")),
        "missing conflict message"
    );
    ensure!(
        ui.by_label("记忆正文")?.value() == Some("Unsaved local proposal."),
        "conflict discarded the draft"
    );
    let after = db.context(&key, None)?;
    ensure!(
        after.len() == 1 && after[0]["id"] == other,
        "conflict overwrote the winner"
    );
    ui.checkpoint("conflict-draft")?;
    ui.click("cancel-edit")?;
    ui.click("refresh")?;
    ensure!(
        ui.element(&format!("note-{other}"))?.visible(),
        "winning version missing after refresh"
    );
    Ok(())
}

fn handoff(ui: &mut Driver, ws: &Workspace, run: &str) -> Result<()> {
    let record = checked_run(ws, run)?;
    let produced = record["memory_outputs"]["implementer"]
        .as_array()
        .context("no implementer memory outputs")?;
    let received = record["memory_inputs"]["reviewer"]
        .as_array()
        .context("no reviewer memory inputs")?;
    let snapshot = Snapshot::load(ws);
    let note = snapshot
        .notes
        .iter()
        .find(|note| {
            note["kind"] == "handoff"
                && note["author"] == "implementer"
                && note["run_id"] == run
                && produced.contains(&note["id"])
                && received.contains(&note["id"])
        })
        .context("reviewer did not receive this run's implementer handoff")?;
    let id = note["id"].as_i64().context("handoff note ID")?;
    let body = note["body"].as_str().context("handoff note body")?;
    for key in ["task", "spec_sha", "candidate_sha"] {
        ensure!(note[key] == record[key], "handoff {key} differs from run");
    }

    ui.click("search")?;
    ui.input(run)?;
    ui.click(&format!("run-{run}"))?;
    ensure!(
        ui.element(&format!("memory_outputs-implementer-{id}"))?
            .visible(),
        "missing implementer output"
    );
    let link = format!("memory_inputs-reviewer-{id}");
    ui.click(&link)?;
    ensure!(
        ui.element("note-body")?.label() == Some(body),
        "reviewer link resolved to the wrong note"
    );
    ui.checkpoint("handoff-note")?;
    ui.click("origin-run")?;
    ensure!(
        ui.element(&link)?.visible(),
        "origin link did not return to the source run"
    );
    ui.checkpoint("handoff-run")?;
    let detail = format!("detail-{run}");
    for artifact in [Artifact::Checks, Artifact::Review] {
        let file = artifact.filename();
        let artifact =
            preview(&ws.data, run, artifact)?.with_context(|| format!("missing {file}"))?;
        let text = if artifact.truncated {
            format!("仅显示末尾 48 KiB\n\n{}", artifact.text)
        } else if artifact.text.trim().is_empty() {
            "文件为空".to_owned()
        } else {
            artifact.text
        };
        ui.reveal(&detail, file)?;
        ui.click(file)?;
        ui.reveal(&detail, "artifact-body")?;
        let actual = ui.element("artifact-body")?;
        ensure!(
            actual.visible() && actual.label() == Some(text.as_str()),
            "wrong preview for {file}"
        );
    }
    ui.checkpoint("handoff-review")
}

fn checked_run(ws: &Workspace, id: &str) -> Result<Value> {
    let record = status(&ws.data, id)?;
    ensure!(
        record["status"] == "awaiting_human_review"
            && record["implementation_source"] == "codex"
            && record["reviewer"] == "claude"
            && record["review_status"] == "findings_recorded"
            && record["check_exit_code"] == 0
            && record["approved"] == false,
        "{id} must have a successful implementation, checks and review awaiting human approval"
    );
    Ok(record)
}

fn continuation(
    ui: &mut Driver,
    ws: &Workspace,
    root: &Path,
    clients: Clients<'_>,
    reference: Option<&str>,
) -> Result<Option<Value>> {
    use handoff_fixture::{DECISION, FOLLOW_UP, RUN};

    let first = status(&ws.data, RUN)?;
    let snapshot = Snapshot::load(ws);
    let question = snapshot
        .notes
        .iter()
        .find(|note| {
            note["run_id"] == RUN
                && note["kind"] == "open_question"
                && note["scope"] == "task"
                && note["author"] == "implementer"
        })
        .context("implementer did not leave a task-level question")?;
    let original = question["id"].as_i64().context("question ID")?;
    let question_body = question["body"].as_str().context("question body")?;
    let decision_body = match reference {
        Some(reference) => format!(
            "Round future fractional seconds to the nearest millisecond, with ties to even. Decision reference {reference}."
        ),
        None => DECISION.into(),
    };
    let question_link = format!("memory_inputs-reviewer-{original}");
    ui.reveal(&format!("detail-{RUN}"), &question_link)?;
    ui.click(&question_link)?;
    ensure!(
        ui.element("note-body")?.label() == Some(question_body),
        "wrong question"
    );
    ui.click("revise-note")?;
    ui.click("决策")?;
    focus_body(ui)?;
    ui.replace_focused(&decision_body)?;
    if reference.is_some() {
        ui.checkpoint("studio-revision")?;
    }
    ui.click("save-note")?;

    let revised = Snapshot::load(ws)
        .notes
        .into_iter()
        .find(|note| note["replaces"] == original && note["kind"] == "decision")
        .context("missing saved decision")?;
    let decision = revised["id"].as_i64().context("decision ID")?;
    ensure!(
        ui.element("note-body")?.label() == Some(decision_body.as_str())
            && revised["body"] == decision_body
            && revised["author"] == "manual"
            && revised["scope"] == question["scope"]
            && revised["spec_sha"] == question["spec_sha"],
        "wrong decision"
    );
    ensure!(
        ui.absent(&format!("read-run-{RUN}"))?,
        "first run linked to the new version"
    );

    println!("Running {FOLLOW_UP} after Studio saved decision #{decision}");
    handoff_fixture::run(root, clients, FOLLOW_UP)?;
    let next = checked_run(ws, FOLLOW_UP)?;
    ensure!(
        next["spec_sha"] == first["spec_sha"] && next["candidate_sha"] != first["candidate_sha"],
        "follow-up did not complete on a new candidate for the same spec"
    );
    let old_outputs = first["memory_outputs"]
        .as_object()
        .context("missing first run outputs")?;
    for role in ["implementer", "reviewer"] {
        let reads = next["memory_inputs"][role]
            .as_array()
            .context("missing follow-up reads")?;
        ensure!(
            reads.iter().any(|id| *id == decision)
                && !old_outputs
                    .values()
                    .filter_map(|ids| ids.as_array())
                    .flatten()
                    .any(|id| reads.contains(id)),
            "follow-up {role} read the wrong memory versions"
        );
    }
    ensure!(
        status(&ws.data, RUN)? == first,
        "first run evidence changed"
    );
    if let Some(reference) = reference {
        handoff_fixture::verify_reference(ws, &next, reference)?;
    }

    ui.click("refresh")?;
    let reader_link = format!("read-run-{FOLLOW_UP}");
    ui.reveal(&format!("detail-memory-{decision}"), &reader_link)?;
    ensure!(
        ui.element(&format!("read-roles-{FOLLOW_UP}"))?.label() == Some("实现 Agent、审查 Agent"),
        "decision missing follow-up reader roles"
    );
    ui.checkpoint("continuation-decision")?;
    ui.click(&reader_link)?;
    let next_link = format!("memory_inputs-reviewer-{decision}");
    ensure!(
        ui.element(&next_link)?.visible()
            && ui
                .element(&format!("memory_inputs-implementer-{decision}"))?
                .visible(),
        "decision link did not open follow-up run"
    );
    ui.checkpoint("continuation-run")?;
    ui.click(&next_link)?;
    let previous_link = format!("replaces-{original}");
    ui.reveal(&format!("detail-memory-{decision}"), &previous_link)?;
    ui.click(&previous_link)?;
    ensure!(
        ui.element("note-body")?.label() == Some(question_body)
            && ui.absent(&reader_link)?
            && ui.element(&format!("read-roles-{RUN}"))?.label() == Some("审查 Agent"),
        "first run no longer linked to its original question"
    );
    ui.reveal(
        &format!("detail-memory-{original}"),
        &format!("read-run-{RUN}"),
    )?;
    ui.checkpoint("continuation-question")?;
    ui.click(&format!("read-run-{RUN}"))?;
    ensure!(
        ui.element(&question_link)?.visible(),
        "question link did not open first run"
    );
    if let Some(reference) = reference {
        let dir = ui
            .args
            .output_dir
            .as_ref()
            .context("live relay output directory")?;
        for (file, evidence) in [
            ("question.json", question.clone()),
            ("decision.json", revised),
        ] {
            fs::write(dir.join(file), serde_json::to_vec_pretty(&evidence)?)?;
        }
        return Ok(Some(json!({
            "status": "passed", "reference": reference, "question_id": original, "decision_id": decision,
            "first_run": RUN, "follow_up_run": FOLLOW_UP, "follow_up_inputs": next["memory_inputs"],
            "reference_in_implementer_note": true, "reference_in_reviewer_report": true,
            "studio_verified": true, "studio_workspace": "workspace/repo"
        })));
    }
    Ok(None)
}

fn studio_relay(args: &Args) -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let ws = handoff_fixture::prepare(fixture.path(), Clients::Fixture)?;
    let runner = handoff_fixture::studio_runner(fixture.path())?;
    let mut ui = Driver::open_with_runner(&ws, args, runner.clone())?;
    let result = (|| {
        ui.click("两轮接力")?;
        ui.click("relay-task-handoff")?;
        let fail = fixture.path().join("bin/fail-check");
        fs::write(&fail, [])?;
        ui.click("start-relay")?;
        ensure!(
            ui.element("relay-first-failure")?
                .label()
                .is_some_and(|error| error.contains("check failed")),
            "the failed first round did not show its cause"
        );
        ensure!(
            ui.absent("relay-resolve-1")?,
            "a rejected candidate published memory"
        );
        ui.checkpoint("studio-relay-failed")?;
        fs::remove_file(&fail)?;
        ui.reveal("relay-detail", "retry-relay-first")?;
        ui.click("retry-relay-first")?;
        let store = ws.data.join("relays");
        let entry = fs::read_dir(store)?.next().context("no saved relay")??;
        let state = aep::workspace::read(&entry.path().join("relay.json"))?;
        let id = state["id"].as_str().context("relay ID")?;
        let failed = state["attempts"][0]["id"]
            .as_str()
            .context("failed run ID")?;
        let first = state["attempts"][1]["id"]
            .as_str()
            .context("first run ID")?;
        let failure = status(&ws.data, failed)?;
        ensure!(
            failure["check_exit_code"] != 0 && failure["memory_outputs"] == json!({}),
            "the failed attempt lost its evidence or published memory"
        );
        if let Some(worktree) = failure["recovery_worktree"].as_str() {
            git(&ws.root, &["worktree", "remove", "--force", worktree])?;
        }
        let first_run = checked_run(&ws, first)?;
        let question = first_run["memory_outputs"]["implementer"][1]
            .as_i64()
            .context("first question")?;

        let mut ui = Driver::open_with_runner(&ws, args, runner)?;
        ui.click("两轮接力")?;
        ui.click(id)?;
        ensure!(
            ui.element("relay-first-evidence")?.visible(),
            "first run evidence is missing after reopening Studio"
        );
        let resolve = format!("relay-resolve-{question}");
        ui.reveal("relay-detail", &resolve)?;
        ui.click(&resolve)?;
        ensure!(
            ui.by_label("记忆正文")?
                .value()
                .is_some_and(|body| body.contains("rounding rule")),
            "Studio did not open the first question"
        );
        ui.replace_focused(handoff_fixture::DECISION)?;
        ui.click("save-note")?;
        let decision = Snapshot::load(&ws)
            .notes
            .into_iter()
            .find(|note| note["replaces"] == question)
            .context("decision after revision")?;
        ensure!(
            decision["kind"] == "decision",
            "revision did not become a decision"
        );
        let decision = decision["id"].as_i64().context("decision ID")?;
        let advance = format!("relay-continue-{decision}");
        ui.reveal("relay-detail", &advance)?;
        ui.checkpoint("studio-relay-decision")?;
        ui.click(&advance)?;
        let state = aep::workspace::read(&entry.path().join("relay.json"))?;
        let second = state["attempts"][2]["id"]
            .as_str()
            .context("second run ID")?;
        let second_run = checked_run(&ws, second)?;
        ensure!(
            first_run["spec_sha"] == second_run["spec_sha"]
                && first_run["candidate_sha"] == second_run["requested_candidate_sha"]
                && second_run["candidate_sha"] != first_run["candidate_sha"]
                && second_run["request"]["require_memory"] == decision,
            "second round was not launched with the first candidate and selected decision"
        );
        for role in ["implementer", "reviewer"] {
            let reads = second_run["memory_inputs"][role]
                .as_array()
                .context("second round memory inputs")?;
            ensure!(
                reads.iter().any(|note| *note == decision)
                    && reads.iter().all(|note| *note != question),
                "{role} did not receive the revised decision"
            );
        }
        ui.reveal("relay-detail", "relay-second-evidence")?;
        ui.checkpoint("studio-relay-complete")?;
        ui.click("relay-second-evidence")?;
        let link = format!("memory_inputs-reviewer-{decision}");
        ui.click(&link)?;
        ensure!(
            ui.element("note-body")?.label() == Some(handoff_fixture::DECISION),
            "second round evidence linked to the wrong memory"
        );
        ui.click("两轮接力")?;
        ui.reveal("relay-detail", "relay-first-evidence")?;
        ui.click("relay-first-evidence")?;
        ensure!(
            ui.element(&format!("memory_outputs-implementer-{question}"))?
                .visible(),
            "first run evidence no longer links to its question"
        );
        Ok(())
    })();
    if result.is_err() {
        let _ = ui.checkpoint("failure");
    }
    result
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Step {
    Click(String),
    ClickLabel(String),
    Input(String),
    Press(String),
    Scroll { id: String, dy: f32 },
    Present(String),
    Absent(String),
    ExpectValue { label: String, equals: String },
}

impl Step {
    fn execute(&self, ui: &mut Driver) -> Result<()> {
        match self {
            Step::Click(id) => ui.click(id)?,
            Step::ClickLabel(label) => {
                let target = ui.by_label(label)?;
                ui.click_target(target)?;
            }
            Step::Input(text) => ui.input(text)?,
            Step::Press(key) => {
                ui.frame(|_, cx| gpui_kit::Keystroke::parse_with_os(key, cx.platform_info().os))??;
                ui.frame(|w, cx| w.press(key, cx))?;
                ui.settle()?;
            }
            Step::Scroll { id, dy } => ui.scroll(id, *dy)?,
            Step::Present(id) => ensure!(ui.element(id)?.visible(), "control is offscreen"),
            Step::Absent(id) => ensure!(ui.absent(id)?, "control is still present"),
            Step::ExpectValue { label, equals } => {
                let actual = ui.by_label(label)?;
                ensure!(
                    actual.value() == Some(equals.as_str()),
                    "actual value: {:?}",
                    actual.value()
                );
            }
        }
        Ok(())
    }
}

fn replay(ui: &mut Driver, steps: &[Step]) -> Result<()> {
    ensure!(!steps.is_empty(), "script contains no steps");
    ui.checkpoint("script-000")?;
    for (i, step) in steps.iter().enumerate() {
        step.execute(ui)
            .with_context(|| format!("step {}: {step:?}", i + 1))?;
        ui.checkpoint(&format!("script-{:03}", i + 1))?;
    }
    Ok(())
}

fn with_fixture(
    args: &Args,
    check: impl FnOnce(&mut Driver, &Workspace, &str) -> Result<()>,
) -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (ws, sha) = fixture(dir.path())?;
    inspect(args, &ws, |ui| check(ui, &ws, &sha))
}

fn inspect<T>(
    args: &Args,
    ws: &Workspace,
    check: impl FnOnce(&mut Driver) -> Result<T>,
) -> Result<T> {
    let mut ui = Driver::open(ws, args)?;
    let result = check(&mut ui);
    if result.is_err()
        && let Err(error) = ui.checkpoint("failure")
    {
        eprintln!("Failed to capture diagnostics: {error:#}");
    }
    result
}

fn relay(args: &Args, clients: Clients<'_>, reference: Option<&str>) -> Result<()> {
    use handoff_fixture::RUN;

    let output = args.output_dir.as_deref().filter(|_| reference.is_some());
    let temporary;
    let root = if let Some(output) = output {
        let root = output.join("workspace");
        fs::create_dir(&root)?;
        root
    } else {
        temporary = tempfile::tempdir()?;
        temporary.path().to_path_buf()
    };
    let result = (|| {
        let ws = handoff_fixture::prepare(&root, clients)?;
        if output.is_some() {
            println!("Studio workspace: {}", ws.root.display());
        }
        println!("Running {RUN}");
        let verified = handoff_fixture::run(&root, clients, RUN).and_then(|()| {
            inspect(args, &ws, |ui| {
                handoff(ui, &ws, RUN)?;
                continuation(ui, &ws, &root, clients, reference)
            })
        });
        if let Some(output) = output {
            let exported = handoff_fixture::export_runs(&ws, output);
            if let Err(error) = &exported {
                eprintln!("Failed to save run evidence: {error:#}");
            }
            let summary = verified?;
            exported?;
            let summary = summary.context("missing live relay verification summary")?;
            let mut file = tempfile::NamedTempFile::new_in(output)?;
            serde_json::to_writer_pretty(&mut file, &summary)?;
            file.persist(output.join("verification.json"))?;
        } else {
            verified?;
        }
        Ok(())
    })();
    if let Err(error) = &result
        && let Some(output) = output
    {
        let diagnostic = format!(
            "{error:#}\nSynthetic workspace retained at {}\n",
            root.join("repo").display()
        );
        eprintln!("{diagnostic}");
        fs::write(output.join("failure.txt"), diagnostic).context("save live relay diagnostics")?;
    }
    result
}

fn main() -> Result<()> {
    let mut args = Args::parse();
    if let Some(dir) = &args.output_dir {
        fs::create_dir_all(dir)?;
    }
    if args.live_relay {
        let output = args
            .output_dir
            .as_ref()
            .context("live relay output directory")?;
        let output = tempfile::Builder::new()
            .prefix("live-relay-")
            .tempdir_in(output)?
            .keep();
        println!("Live relay evidence: {}", output.display());
        args.output_dir = Some(output);
        args.screenshots = true;
        let token = tempfile::Builder::new()
            .prefix("decision-")
            .rand_bytes(16)
            .tempfile()?;
        let reference = token
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .context("decision reference")?;
        let clients = Clients::Live {
            model: args.codex_model.as_deref(),
        };
        relay(&args, clients, Some(reference))?;
    } else if let (Some(repo), Some(run)) = (&args.repo, &args.run_id) {
        let ws = Workspace::open(repo)?;
        inspect(&args, &ws, |ui| handoff(ui, &ws, run))?;
    } else if let Some(path) = &args.script {
        let steps: Vec<Step> = serde_json::from_slice(
            &fs::read(path).with_context(|| format!("read {}", path.display()))?,
        )
        .context("parse UI steps")?;
        with_fixture(&args, |ui, _, _| replay(ui, &steps))?;
    } else {
        let scenarios = match &args.scenario {
            Some(s) => std::slice::from_ref(s),
            None => &[
                Scenario::History,
                Scenario::Conflict,
                Scenario::Handoff,
                Scenario::StudioRelay,
            ],
        };
        for scenario in scenarios {
            match scenario {
                Scenario::History => with_fixture(&args, history)?,
                Scenario::Conflict => with_fixture(&args, |ui, ws, _| conflict(ui, ws))?,
                Scenario::Handoff => relay(&args, Clients::Fixture, None)?,
                Scenario::StudioRelay => studio_relay(&args)?,
            }
        }
    }
    println!("UI checks passed.");
    Ok(())
}
