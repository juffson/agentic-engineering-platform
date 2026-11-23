//! Run Studio's real event handlers in an offscreen GPUI window.
#[path = "support/handoff.rs"]
mod handoff_fixture;
#[path = "support/https_keys.rs"]
mod https_keys;
#[path = "support/https_pilot.rs"]
mod https_pilot;
#[path = "support/https_queue.rs"]
mod https_queue;
#[path = "support/https_rolling.rs"]
mod https_rolling;
#[path = "support/https_workers.rs"]
mod https_workers;
#[path = "support/lifecycle/mod.rs"]
mod lifecycle;
#[path = "support/local.rs"]
mod local;
#[path = "support/project.rs"]
mod project;
#[path = "support/workflows.rs"]
mod workflows;
#[path = "support/zcode_history.rs"]
mod zcode_history;

use aep::{
    memory::{Key, Kind, Memory},
    studio::{Artifact, Snapshot, preview},
    workspace::{Workspace, git, status},
};
use anyhow::{Context, Result, ensure};
use clap::{ArgGroup, Parser, ValueEnum};
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
enum LiveKeyStep {
    Provision,
    Start,
    Stage,
    Verify,
    ExpectFailure,
    Deliver,
}

#[derive(Clone, Copy, ValueEnum)]
enum Scenario {
    Project,
    Workflows,
    History,
    Conflict,
    Handoff,
    StudioRelay,
    Local,
}

#[derive(Parser)]
#[command(
    about = "Exercise AEP Studio in a temporary Git workspace, using an offscreen GPUI window",
    group(ArgGroup::new("live").args(["live_zcode_history","live_project_workflow","live_relay", "live_lifecycle", "live_https_pilot", "live_https_remote", "live_https_queue", "live_https_workers", "live_https_keys", "live_https_rolling_studio"]))
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
    /// Open project settings in an existing repository, sync ZCode records and open an original task.
    #[arg(long, requires="output_dir", conflicts_with_all=["scenario","script","repo"])]
    live_zcode_history: Option<PathBuf>,
    /// Exercise all seven software stages with the installed Codex, Claude and ZCode clients.
    #[arg(long, requires="output_dir",conflicts_with_all=["scenario","script","repo"])]
    live_project_workflow: bool,
    /// ZCode provider/model or provider/model@reasoning-level for the testing stage.
    #[arg(long, requires = "live_project_workflow")]
    zcode_model: Option<String>,
    /// Run two rounds with real Codex and Claude and capture evidence and PNGs
    #[arg(long, requires = "output_dir", conflicts_with_all = ["scenario", "script", "repo"])]
    live_relay: bool,
    /// Run the full Studio workflow with live clients, GitHub and operator confirmation
    #[arg(long, conflicts_with_all = ["scenario", "script", "repo"])]
    live_lifecycle: bool,
    /// Run the live HTTPS pilot through Studio, requiring both operator confirmations
    #[arg(long, requires = "output_dir", conflicts_with_all = ["scenario", "script", "repo", "codex_model"])]
    live_https_pilot: bool,
    /// Select and run a real task against an already deployed HTTPS control plane
    #[arg(long, requires = "output_dir", conflicts_with_all = ["scenario", "script", "repo", "codex_model"])]
    live_https_remote: bool,
    /// Use this existing GitHub checkout for the live HTTPS remote rehearsal
    #[arg(long, requires = "live")]
    https_remote_repo: Option<PathBuf>,
    /// Rehearse HTTPS queue controls with two existing tasks on the same site.
    #[arg(long, requires = "output_dir", conflicts_with_all = ["scenario", "script", "repo", "codex_model"])]
    live_https_queue: bool,
    /// Verify two HTTPS Workers and an outage scoped to one of them.
    #[arg(long, requires = "output_dir", conflicts_with_all = ["scenario", "script", "repo", "codex_model"])]
    live_https_workers: bool,
    /// Configure or rotate a Deploy Key through a live HTTPS Studio site.
    #[arg(long, requires_all = ["output_dir", "https_remote_repo", "https_key_worker"], conflicts_with_all = ["scenario", "script", "repo", "codex_model"])]
    live_https_keys: bool,
    /// Start the existing-site rolling-key rehearsal from Studio and confirm its UI gates.
    #[arg(long, requires = "output_dir", conflicts_with_all = ["scenario", "script", "repo", "codex_model"])]
    live_https_rolling_studio: bool,
    #[arg(long, requires = "live_https_keys")]
    https_key_worker: Option<String>,
    /// Confirm retirement of the previously rotated key through Studio.
    #[arg(long, requires = "live_https_keys")]
    https_key_revoke: bool,
    /// Select a checkpoint in the live rolling Deploy Key rehearsal.
    #[arg(
        long,
        requires = "live_https_keys",
        conflicts_with = "https_key_revoke"
    )]
    https_key_step: Option<LiveKeyStep>,
    /// Override the Codex model for both rounds of the live relay
    #[arg(long, requires = "live")]
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
            let options = WindowOptions {
                show: false,
                focus: false,
                window_bounds: Some(WindowBounds::centered(size(px(1120.), px(730.)), cx)),
                ..Default::default()
            };
            if args.live_zcode_history.is_some() {
                aep_studio::open_project_window(Some(ws.root.clone()), options, cx)
            } else {
                aep_studio::open_studio_with_runner(Some(ws.clone()), runner, options, cx)
            }
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
            && matches!(
                record["implementation_source"].as_str(),
                Some("codex" | "zcode")
            )
            && matches!(record["reviewer"].as_str(), Some("claude" | "zcode"))
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
        handoff_fixture::verify_reference(ws, FOLLOW_UP, &next, reference)?;
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
    git(&ws.root, &["branch", "-M", "main"])?;
    git(&ws.root, &["push", "origin", "HEAD:refs/heads/main"])?;
    let home = ws.data.join("studio-projects");
    let mut library = aep::project::Library::open(&home)?;
    library.change(&home, |lib| {
        lib.create("relay defaults", vec![ws.root.clone()])?;
        lib.project_mut()?.set_zcode_default("team/qa@high".into());
        Ok(())
    })?;
    let mut ui = Driver::open_with_runner(&ws, args, runner.clone())?;
    let result = (|| {
        ui.click("两轮接力")?;
        ui.click("relay-task-handoff")?;
        ui.click("relay-reviewer-zcode")?;
        ensure!(
            ui.by_label("ZCode 模型")?.value() == Some("team/qa@high"),
            "first round default missing"
        );
        ui.click("round-zcode-provider-0")?;
        ui.click("round-zcode-level-0")?;
        ensure!(
            ui.by_label("ZCode 模型")?.value() == Some("team/qa@low"),
            "first round override missing"
        );
        ui.click("relay-reviewer-claude")?;
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
        let first_run = checked_run(&ws, first)?;
        let question = first_run["memory_outputs"]["implementer"][1]
            .as_i64()
            .context("first question")?;

        ui = Driver::open_with_runner(&ws, args, runner)?;
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
        for control in ["relay-implementer-zcode", "relay-reviewer-zcode"] {
            ui.reveal("relay-detail", control)?;
            ui.click(control)?;
        }
        ensure!(
            !ui.absent("relay-zcode-model")? && ui.absent("relay-model")?,
            "wrong model fields for two ZCode clients"
        );
        ensure!(
            ui.by_label("ZCode 模型")?.value() == Some("team/qa@high"),
            "first round override leaked into second"
        );
        ui.reveal("relay-detail", "round-zcode-native")?;
        ui.click("round-zcode-native")?;
        ensure!(
            aep::project::Library::open(&home)?
                .project()
                .unwrap()
                .zcode_default()
                == "team/qa@high",
            "round changed project defaults"
        );
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
            second_run["request"]["source"] == "zcode" && second_run["reviewer"] == "zcode",
            "second round ignored Studio client selection"
        );
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
        verify_publication(&mut ui, &ws, fixture.path(), second, &second_run)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = ui.checkpoint("failure");
    }
    result
}

fn verify_publication(
    ui: &mut Driver,
    ws: &Workspace,
    fixture: &Path,
    second: &str,
    record: &Value,
) -> Result<()> {
    let reference = format!("refs/heads/aep/pr/handoff/{second}");
    ui.click("两轮接力")?;
    ui.reveal("relay-detail", "open-human-review")?;
    ui.click("open-human-review")?;
    ensure!(
        ui.element("review-content")?
            .label()
            .is_some_and(|s| { s.contains("diff --git a/normalize.rs") && s.contains("* 1000") }),
        "final review did not show the candidate diff"
    );
    ui.click("review-checks")?;
    ensure!(
        ui.element("review-content")?
            .label()
            .is_some_and(|s| s.contains("test result: ok")),
        "final review did not show the acceptance log"
    );
    ui.click("review-report")?;
    ensure!(
        ui.element("review-content")?.label()
            == Some("Reviewed the conversion and revised decision.\n"),
        "final review did not show the second-round ZCode report"
    );
    ui.click("review-diff")?;
    ensure!(
        ui.absent("publish-confirmed")?
            && git(&ws.root, &["ls-remote", "--refs", "origin", &reference])?.is_empty()
            && !fixture.join("pr-posts").exists(),
        "opening final review mutated GitHub"
    );
    ui.checkpoint("studio-review-ready")?;

    git(
        &ws.root,
        &["push", "origin", "HEAD:refs/heads/review-sandbox"],
    )?;
    ui.click("acknowledge-review")?;
    ui.click("review-target-input")?;
    ui.input("review-sandbox")?;
    ensure!(
        ui.absent("publish-confirmed")? && ui.absent("acknowledge-review")?,
        "editing the PR target kept an old confirmation"
    );
    ui.click("load-pr-target")?;
    ensure!(
        ui.element("review-target-branch")?.label() == Some("review-sandbox")
            && ui.absent("publish-confirmed")?,
        "the new review did not bind the requested branch"
    );
    ui.click("review-target-input")?;
    ui.frame(|window, cx| {
        window.press("cmd-a", cx);
        window.press("backspace", cx);
    })?;
    ui.settle()?;
    ui.click("load-pr-target")?;
    ensure!(
        ui.element("review-target-branch")?.label() == Some("main"),
        "empty review target did not load the GitHub default"
    );

    git(
        &ws.root,
        &["commit", "--allow-empty", "-m", "Advance target"],
    )?;
    git(&ws.root, &["push", "origin", "HEAD:refs/heads/main"])?;
    ui.click("acknowledge-review")?;
    ui.click("publish-confirmed")?;
    ensure!(
        ui.element("publish-error")?
            .label()
            .is_some_and(|s| s.contains("目标分支已更新"))
            && git(&ws.root, &["ls-remote", "--refs", "origin", &reference])?.is_empty(),
        "the stale review pushed a candidate"
    );

    ui.click("refresh-human-review")?;
    ensure!(
        ui.absent("publish-confirmed")?,
        "reloaded review reused stale confirmation"
    );
    let fail = fixture.join("bin/fail-pr");
    fs::write(&fail, [])?;
    ui.click("acknowledge-review")?;
    ui.click("publish-confirmed")?;
    ensure!(
        ui.element("publish-receipt-error")?
            .label()
            .is_some_and(|s| s.contains("simulated GitHub outage")),
        "PR creation failure was not displayed"
    );
    let publication = ws.data.join("runs").join(second).join("publication.json");
    let pushed = aep::workspace::read(&publication)?;
    ensure!(
        pushed["pushed"] == true && pushed["pr"].is_null(),
        "partial publication was not saved"
    );
    fs::remove_file(fail)?;
    ui.click("back-to-relay")?;
    ui.reveal("relay-detail", "open-human-review")?;
    ui.click("open-human-review")?;
    ensure!(
        ui.absent("publish-confirmed")?,
        "reopening the review bypassed human confirmation"
    );
    ui.click("acknowledge-review")?;
    ui.click("publish-confirmed")?;
    ensure!(
        ui.element("open-created-pr")?.visible(),
        "created PR link is missing"
    );
    let saved = aep::workspace::read(&publication)?;
    ensure!(
        saved["pr"]["number"] == 17
            && saved["pr"]["head_sha"] == record["candidate_sha"]
            && saved["confirmed_by"] == "aep-reviewer"
            && saved["error"].is_null()
            && status(&ws.data, second)? == *record,
        "the saved PR does not match the confirmed run"
    );
    ui.checkpoint("studio-review-published")?;
    ui.click("back-to-relay")?;
    ensure!(
        ui.element("relay-created-pr")?.visible(),
        "relay did not show the PR"
    );
    ui.reveal("relay-detail", "open-human-review")?;
    ui.click("open-human-review")?;
    ensure!(
        ui.element("open-created-pr")?.visible()
            && ui.absent("publish-confirmed")?
            && fs::read_to_string(fixture.join("pr-posts"))? == "POST\nPOST\n",
        "reopening the published run tried to create another PR"
    );
    verify_github_merge(ui, ws, fixture, second, record)?;
    Ok(())
}

fn verify_github_merge(
    ui: &mut Driver,
    ws: &Workspace,
    fixture: &Path,
    second: &str,
    record: &Value,
) -> Result<()> {
    ui.click("track-created-pr")?;
    ensure!(
        ui.element("github-merge-blocker")?.label() == Some("GitHub 检查仍在运行")
            && ui.absent("github-confirm-merge")?,
        "running CI exposed merge"
    );
    let mode = fixture.join("ci-state");
    fs::write(&mode, "failed")?;
    ui.click("refresh-github-pr")?;
    ensure!(
        ui.absent("github-confirm-merge")?,
        "failed CI exposed merge"
    );
    ui.click("github-failure-0")?;
    ensure!(
        ui.element("github-check-diagnostic")?
            .label()
            .is_some_and(|text| {
                text.contains("Test expected a widened result.")
                    && text.contains("assertion failed: upper bound")
            }),
        "missing GitHub annotation or failure log"
    );
    ui.checkpoint("studio-github-ci-failed")?;
    ui.reveal("github-merge-card", "github-retry-checks")?;
    ui.click("github-retry-checks")?;
    ensure!(
        fs::read_to_string(fixture.join("ci-reruns"))? == "rerun\n"
            && ui.element("github-merge-blocker")?.label() == Some("GitHub 检查仍在运行")
            && ui.absent("github-confirm-merge")?,
        "retry did not submit the saved candidate's failed Actions run"
    );
    ui.click("github-review-tab")?;
    ensure!(
        ui.element("github-note-0")?.label() == Some("Please keep the overflow regression test.")
            && ui.element("github-note-1")?.label() == Some("Review fixture comment."),
        "GitHub review or discussion is missing"
    );
    fs::write(&mode, "changes")?;
    ui.click("refresh-github-pr")?;
    ensure!(
        ui.element("github-merge-blocker")?.label() == Some("GitHub 仍要求审查或修改"),
        "required changes did not block merge"
    );

    fs::write(&mode, "passed")?;
    ui.click("refresh-github-pr")?;
    ensure!(
        ui.element("github-confirm-merge")?.visible()
            && ui.absent("github-merge-now")?
            && !fixture.join("pr-merges").exists(),
        "checks alone caused merge"
    );
    ui.click("github-confirm-merge")?;
    fs::write(&mode, "pending")?;
    ui.click("github-merge-now")?;
    ensure!(
        ui.element("github-pr-error")?
            .label()
            .is_some_and(|text| text.contains("请刷新后重新确认"))
            && !fixture.join("pr-merges").exists(),
        "a stale confirmation sent a merge"
    );
    fs::write(&mode, "passed")?;
    ui.click("refresh-github-pr")?;
    ensure!(
        ui.absent("github-merge-now")?,
        "refresh reused a stale approval"
    );
    ui.checkpoint("studio-github-merge-ready")?;
    ui.click("github-confirm-merge")?;
    ui.click("github-merge-now")?;
    ensure!(
        ui.element("github-merged")?.visible(),
        "missing merge result"
    );
    let receipt = aep::workspace::read(&ws.data.join("runs").join(second).join("merge.json"))?;
    ensure!(
        receipt["head"] == record["candidate_sha"]
            && receipt["merged_sha"] == fs::read_to_string(fixture.join("merged-sha"))?.trim()
            && receipt["confirmed_by"] == "aep-reviewer",
        "merge receipt does not match the confirmed head"
    );
    ui.checkpoint("studio-github-merged")?;
    ui.click("github-back-to-relay")?;
    ensure!(
        ui.element("relay-second-progress")?.label() == Some("GitHub PR 已合并"),
        "relay did not show the merged state"
    );
    ui.reveal("relay-detail", "relay-pr-progress")?;
    ui.click("relay-pr-progress")?;
    ensure!(
        ui.element("github-merged")?.visible()
            && ui.absent("github-merge-now")?
            && fs::read_to_string(fixture.join("pr-merges"))? == "PUT\n",
        "reopening a merged PR submitted another merge"
    );
    verify_post_merge_cleanup(ui, ws, fixture, second, record)?;
    Ok(())
}

fn verify_post_merge_cleanup(
    ui: &mut Driver,
    ws: &Workspace,
    fixture: &Path,
    second: &str,
    record: &Value,
) -> Result<()> {
    let folder = ws.data.join("runs").join(second);
    let merge = fs::read(folder.join("merge.json"))?;
    let merged = aep::workspace::read(&folder.join("merge.json"))?;
    let merge_sha = merged["merged_sha"].as_str().context("merge SHA")?;
    let candidate = record["candidate_sha"].as_str().context("candidate SHA")?;
    let head = format!("aep/pr/handoff/{second}");
    let head_ref = format!("refs/heads/{head}");
    let remote = fixture.join("remote.git");
    let relay_dir = fs::read_dir(ws.data.join("relays"))?
        .next()
        .context("relay")??;
    let relay = aep::workspace::read(&relay_dir.path().join("relay.json"))?;
    let failed = relay["attempts"][0]["id"]
        .as_str()
        .context("failed attempt")?;
    let first = relay["attempts"][1]["id"].as_str().context("first run")?;
    let failure = status(&ws.data, failed)?;
    let scratch = PathBuf::from(
        failure["recovery_worktree"]
            .as_str()
            .context("failed worktree")?,
    );
    let before = [status(&ws.data, first)?, status(&ws.data, second)?, failure];
    let missing = before[0]["worktree"]
        .as_str()
        .context("first round worktree")?
        .to_owned();
    let first_sha = before[0]["candidate_sha"]
        .as_str()
        .context("first candidate")?;
    git(
        &ws.root,
        &["worktree", "add", "--detach", &missing, first_sha],
    )?;
    fs::remove_dir_all(&missing)?;
    let memory = fs::read(ws.data.join("memory.sqlite3"))?;
    let original_tip = git(&ws.root, &["rev-parse", "HEAD"])?;
    ensure!(
        scratch.is_dir() && original_tip.trim() != merge_sha,
        "cleanup fixture is already clean"
    );

    let untracked = ws.root.join("keep-local.txt");
    fs::write(&untracked, "unsaved local work")?;
    ui.reveal("github-merge-card", "github-finish-merge")?;
    ui.click("github-finish-merge")?;
    ensure!(
        ui.element("github-cleanup-target")?
            .label()
            .is_some_and(|text| text.contains("未跟踪"))
            && git(&ws.root, &["rev-parse", "HEAD"])? == original_tip
            && git(&remote, &["rev-parse", &head_ref])?.trim() == candidate,
        "a dirty local checkout did not block destructive steps"
    );
    ui.checkpoint("studio-post-merge-blocked")?;
    fs::remove_file(untracked)?;

    let user_file = scratch.join("keep-worktree.txt");
    fs::write(&user_file, "preserve uncommitted worktree content")?;
    git(&remote, &["update-ref", &head_ref, merge_sha, candidate])?;
    ui.reveal("github-merge-card", "github-finish-merge")?;
    ui.click("github-finish-merge")?;
    ensure!(
        ui.element("github-cleanup-branch")?
            .label()
            .is_some_and(|text| text.contains("其他提交"))
            && ui
                .element("github-cleanup-worktree-0")?
                .label()
                .is_some_and(|text| text.contains("未跟踪"))
            && user_file.is_file()
            && git(&ws.root, &["rev-parse", "HEAD"])?.trim() == merge_sha
            && git(&remote, &["rev-parse", &head_ref])?.trim() == merge_sha,
        "changed refs or dirty Worktrees were not preserved"
    );
    fs::remove_file(user_file)?;
    git(&remote, &["update-ref", &head_ref, candidate, merge_sha])?;
    git(&ws.root, &["branch", "--", &head, candidate])?;
    let unrelated = fixture.join("aep-work-unrelated");
    git(
        &ws.root,
        &[
            "worktree",
            "add",
            "--detach",
            unrelated.to_str().context("unrelated worktree")?,
            candidate,
        ],
    )?;
    ui.reveal("github-merge-card", "github-finish-merge")?;
    ui.click("github-finish-merge")?;
    ui.reveal("github-merge-card", "github-cleanup-complete")?;
    ensure!(
        ui.element("github-cleanup-complete")?.visible()
            && !scratch.exists()
            && !git(&ws.root, &["worktree", "list", "--porcelain"])?.contains(
                Path::new(&missing)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .context("missing worktree name")?
            )
            && unrelated.is_dir()
            && git(&ws.root, &["ls-remote", "--refs", "origin", &head_ref])?.is_empty()
            && git(&ws.root, &["rev-parse", &head_ref])?.trim() == candidate,
        "cleanup removed user branches or left the generated branch or worktree"
    );
    ensure!(
        before
            == [
                status(&ws.data, first)?,
                status(&ws.data, second)?,
                status(&ws.data, failed)?
            ]
            && memory == fs::read(ws.data.join("memory.sqlite3"))?
            && merge == fs::read(folder.join("merge.json"))?
            && git(&ws.root, &["rev-parse", &format!("refs/aep/runs/{second}")])?.trim()
                == candidate,
        "post-merge cleanup changed historical evidence, memory or the merge receipt"
    );
    ui.checkpoint("studio-post-merge-complete")?;
    ui.click("github-back-to-relay")?;
    ensure!(
        ui.element("relay-second-progress")?.label() == Some("合并与收尾已完成"),
        "relay did not retain the cleanup receipt"
    );
    ui.reveal("relay-detail", "relay-pr-progress")?;
    ui.click("relay-pr-progress")?;
    ensure!(
        ui.element("github-cleanup-complete").is_ok() && ui.absent("github-finish-merge")?,
        "reopening the PR did not show the completed cleanup"
    );
    Ok(())
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
            let exported =
                handoff_fixture::export_runs(&ws, output, [RUN, handoff_fixture::FOLLOW_UP]);
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
    if args.live_https_rolling_studio {
        return https_rolling::run(args);
    }
    if args.live_https_keys {
        return https_keys::run(args);
    }
    if args.live_https_workers {
        return https_workers::run(args);
    }
    if args.live_https_queue {
        return https_queue::run(args);
    }
    if args.live_https_remote {
        return https_pilot::run_remote(args);
    }
    if args.live_https_pilot {
        return https_pilot::run(args);
    }
    if args.live_lifecycle {
        return lifecycle::run(args);
    }
    if let Some(dir) = &args.output_dir {
        fs::create_dir_all(dir)?;
    }
    if let Some(path) = &args.live_zcode_history {
        let ws = Workspace::open(path)?;
        return inspect(&args, &ws, |ui| zcode_history::exercise(ui, &ws.root));
    }
    if args.live_project_workflow {
        return workflows::live(&mut args);
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
                Scenario::Local,
                Scenario::Project,
                Scenario::Workflows,
            ],
        };
        for scenario in scenarios {
            match scenario {
                Scenario::History => with_fixture(&args, history)?,
                Scenario::Conflict => with_fixture(&args, |ui, ws, _| conflict(ui, ws))?,
                Scenario::Handoff => relay(&args, Clients::Fixture, None)?,
                Scenario::StudioRelay => studio_relay(&args)?,
                Scenario::Workflows => workflows::run(&args)?,
                Scenario::Project => project::run(&args)?,
                Scenario::Local => local::run(&args)?,
            }
        }
    }
    println!("UI checks passed.");
    Ok(())
}
