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
use serde_json::json;
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
}

struct Driver<'a> {
    cx: HeadlessAppContext,
    window: AnyWindowHandle,
    args: &'a Args,
}

impl<'a> Driver<'a> {
    fn open(ws: &Workspace, args: &'a Args) -> Result<Self> {
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
            aep_studio::open_studio(
                Some(ws.clone()),
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
    let record = status(&ws.data, run)?;
    ensure!(
        record["status"] == "awaiting_human_review"
            && record["implementation_source"] == "codex"
            && record["review_status"] == "findings_recorded"
            && record["check_exit_code"] == 0
            && record["approved"] == false,
        "{run} must have a successful implementation, checks and review awaiting human approval"
    );
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

fn inspect(
    args: &Args,
    ws: &Workspace,
    check: impl FnOnce(&mut Driver) -> Result<()>,
) -> Result<()> {
    let mut ui = Driver::open(ws, args)?;
    let result = check(&mut ui);
    if result.is_err()
        && let Err(error) = ui.checkpoint("failure")
    {
        eprintln!("Failed to capture diagnostics: {error:#}");
    }
    result
}

fn main() -> Result<()> {
    let args = Args::parse();
    if let Some(dir) = &args.output_dir {
        fs::create_dir_all(dir)?;
    }
    if let (Some(repo), Some(run)) = (&args.repo, &args.run_id) {
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
            None => &[Scenario::History, Scenario::Conflict, Scenario::Handoff],
        };
        for scenario in scenarios {
            match scenario {
                Scenario::History => with_fixture(&args, history)?,
                Scenario::Conflict => with_fixture(&args, |ui, ws, _| conflict(ui, ws))?,
                Scenario::Handoff => {
                    let dir = tempfile::tempdir()?;
                    let ws = handoff_fixture::run(dir.path())?;
                    inspect(&args, &ws, |ui| handoff(ui, &ws, handoff_fixture::RUN))?;
                }
            }
        }
    }
    println!("UI checks passed.");
    Ok(())
}
