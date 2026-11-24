//! T6 · runner end to end: a test-double `claude` runs 产品需求 → 交互设计 → 技术方案.
//!
//! Covers AC-4 (resume limit, both branches), AC-5 (each stage keeps its own result,
//! evidence and receipt), AC-6 (adjacent reuse: `--resume … --fork-session`, short prompt),
//! AC-7 (a different model starts a new session) and AC-8 (window usage per stage).
use aep::{
    handoff::{Assignment, READING},
    project::{Kind, Library},
    workflow::{Client, Profile, Template},
};
use anyhow::{Result, ensure};
use serde_json::Value;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

/// A fake Claude Code: answers `--help` like a current version, logs every call's argv and
/// stdin under `log/`, and prints a stream-json run whose last call reads `log/ctx` tokens.
/// Without `--fork-session`, `--resume <id>` keeps the session ID; otherwise each call gets
/// a new one (`s-<n>`).
fn fake_claude(root: &Path) -> Result<(PathBuf, PathBuf)> {
    let log = root.join("log");
    fs::create_dir_all(&log)?;
    let bin = root.join("claude");
    let script = r#"#!/bin/sh
set -eu
LOG='@LOG@'
if [ "${1:-}" = "--help" ]; then
  printf '%s\n' 'Usage: claude [options] [command] [prompt]' \
    '  --strict-mcp-config      Only use MCP servers from --mcp-config' \
    '  --setting-sources <s>    Setting sources to load' \
    '  --disable-slash-commands Disable all skills' \
    '  --settings <file-or-json> Additional settings' \
    '  -r, --resume [value]     Resume a conversation' \
    '  --fork-session           New session ID when resuming'
  exit 0
fi
n=$(( $(cat "$LOG/count" 2>/dev/null || echo 0) + 1 ))
echo "$n" > "$LOG/count"
printf '%s\n' "$@" > "$LOG/argv-$n"
cat > "$LOG/stdin-$n"
ctx=$(cat "$LOG/ctx" 2>/dev/null || echo 9000)
rest=$((ctx - 10))
res=""; fork=0; prev=""
for a in "$@"; do
  if [ "$prev" = "--resume" ]; then res="$a"; fi
  if [ "$a" = "--fork-session" ]; then fork=1; fi
  prev="$a"
done
sid="s-$n"
if [ -n "$res" ] && [ "$fork" = 0 ]; then sid="$res"; fi
printf '%s\n' "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"$sid\",\"model\":\"fake\",\"tools\":[\"Read\",\"Grep\",\"Glob\",\"Bash\"],\"mcp_servers\":[],\"skills\":[],\"plugins\":[{\"name\":\"agents-md\",\"path\":\"@builtin\"}],\"claude_code_version\":\"9.9.9\"}"
printf '%s\n' "{\"type\":\"assistant\",\"message\":{\"id\":\"m$n-a\",\"usage\":{\"input_tokens\":5,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":3000,\"output_tokens\":2},\"content\":[{\"type\":\"text\",\"text\":\"看一下\"}]}}"
printf '%s\n' "{\"type\":\"assistant\",\"message\":{\"id\":\"m$n-b\",\"usage\":{\"input_tokens\":10,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":$rest,\"output_tokens\":4},\"content\":[{\"type\":\"text\",\"text\":\"第 ${n} 轮产出\"}]}}"
printf '%s\n' "{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"第 ${n} 轮产出 OUT-${n}\\nAEP-HANDOFF: 结论 R${n}；证据 specs/diet/；下一步 下一阶段\",\"is_error\":false,\"session_id\":\"$sid\"}"
"#
    .replace("@LOG@", &log.display().to_string());
    fs::write(&bin, script)?;
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o700))?;
    Ok((bin, log))
}

struct Item {
    home: PathBuf,
    lib: Library,
    wid: String,
    log: PathBuf,
    bin: PathBuf,
    /// Claude user config with no default model, so runs do not depend on this machine (E2).
    config: PathBuf,
    calls: usize,
}

/// One stage run as Studio drives it: reserve, run `aep workflow-stage`, then read back.
struct Run {
    dir: PathBuf,
    ev: Value,
    argv: Vec<String>,
    prompt: String,
}
impl Run {
    fn has(&self, pair: &[&str]) -> bool {
        self.argv.windows(pair.len()).any(|w| w == pair)
    }
    fn result(&self) -> String {
        fs::read_to_string(self.dir.join("result.txt")).unwrap_or_default()
    }
}

impl Item {
    fn new(root: &Path, name: &str) -> Result<Self> {
        let (bin, log) = fake_claude(root)?;
        let home = root.join("catalog");
        let config = root.join("claude-config");
        fs::create_dir_all(&config)?;
        let mut lib = Library::open(&home)?;
        let wid = lib.change(&home, |lib| {
            lib.create(name, vec![])?;
            let p = lib.project_mut()?;
            p.add_work("精简阶段上下文", Kind::Feature, Template::Software)?;
            Ok(p.selected().unwrap().id.clone())
        })?;
        Ok(Item {
            home,
            lib,
            wid,
            log,
            bin,
            config,
            calls: 0,
        })
    }

    /// The context the fake's last call reports for the next run.
    fn context(&self, tokens: u64) -> Result<()> {
        Ok(fs::write(self.log.join("ctx"), tokens.to_string())?)
    }

    fn run(&mut self, stage: usize, model: &str) -> Result<Run> {
        let profile = Profile {
            client: Client::Claude,
            executable: self.bin.display().to_string(),
            model: model.into(),
            check_command: String::new(),
        };
        let (home, wid) = (self.home.clone(), self.wid.clone());
        let dir = self.lib.change(&home, |lib| {
            lib.project_mut()?.reserve(
                &home,
                &wid,
                stage,
                Assignment {
                    profile,
                    brief: format!("第 {stage} 阶段说明"),
                    repository: None,
                    external: None,
                },
            )
        })?;
        let out = Command::new(env!("CARGO_BIN_EXE_aep"))
            .env("CLAUDE_CONFIG_DIR", &self.config)
            .args(["workflow-stage", "--request"])
            .arg(dir.join("request.json"))
            .output()?;
        let ev: Value = serde_json::from_slice(&fs::read(dir.join("evidence.json"))?)?;
        ensure!(
            out.status.success() && ev["status"] == "succeeded",
            "stage {stage}: {ev}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        self.calls += 1;
        let n = self.calls;
        ensure!(
            fs::read_to_string(self.log.join("count"))?.trim() == n.to_string(),
            "stage {stage} should call claude exactly once"
        );
        let argv = fs::read_to_string(self.log.join(format!("argv-{n}")))?
            .lines()
            .map(str::to_owned)
            .collect();
        let prompt = fs::read_to_string(self.log.join(format!("stdin-{n}")))?;
        assert_eq!(
            prompt,
            fs::read_to_string(dir.join("prompt.txt"))?,
            "prompt.txt records what was sent"
        );
        Ok(Run {
            dir,
            ev,
            argv,
            prompt,
        })
    }

    /// The operator accepts the stage; its result becomes the stage note and hand-off.
    fn accept(&mut self, stage: usize) -> Result<()> {
        let (home, wid) = (self.home.clone(), self.wid.clone());
        self.lib.change(&home, |lib| {
            lib.project_mut()?
                .finish_stage(&home, &wid, stage, String::new())
        })
    }
}

fn run_id(r: &Run) -> String {
    r.dir.file_name().unwrap().to_string_lossy().into_owned()
}

/// Every run, reused or not, starts Claude with the minimal launch (AC-1 carried through).
fn assert_minimal(r: &Run) {
    for pair in [
        &["--strict-mcp-config"][..],
        &["--setting-sources", "project,local"],
        &["--disable-slash-commands"],
        &["--settings", r#"{"autoMemoryEnabled":false}"#],
    ] {
        assert!(r.has(pair), "missing {pair:?} in {:?}", r.argv);
    }
    assert_eq!(r.ev["launch"]["diet"], aep::launch::DIET, "{}", r.ev);
    assert_eq!(r.ev["launch"]["minimal"], true, "{}", r.ev);
}

/// AC-8: the stage counts only its own calls; its last call is what the next run compares.
fn assert_window(r: &Run, context: u64) {
    assert_eq!(r.ev["usage_window"]["calls"], 2, "{}", r.ev);
    assert_eq!(r.ev["last_context"], context, "{}", r.ev);
}

#[test]
fn adjacent_stages_fork_the_previous_session_with_a_short_prompt() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut item = Item::new(temp.path(), "diet-chain")?;

    // 产品需求: nothing to reuse.
    let req = item.run(0, "")?;
    assert_minimal(&req);
    assert!(!req.argv.iter().any(|a| a == "--resume"), "{:?}", req.argv);
    assert_eq!(req.ev["fresh_session_reason"], "no_prior", "{}", req.ev);
    assert!(req.ev["reused_session"].is_null(), "{}", req.ev);
    assert_eq!(req.ev["session"], "s-1");
    assert_eq!(req.ev["resumable"], true);
    assert!(req.prompt.contains("阶段：产品需求"));
    assert!(
        req.prompt.contains(READING),
        "R5 reading rules in a fresh prompt"
    );
    assert_window(&req, 9000);
    item.accept(0)?;

    // 交互设计: same CLI, model, permissions and workspace → fork 产品需求's session.
    let ui = item.run(1, "")?;
    assert_minimal(&ui);
    assert!(
        ui.has(&["--resume", "s-1", "--fork-session"]),
        "{:?}",
        ui.argv
    );
    assert_eq!(ui.ev["session"], "s-2", "the fork has its own session ID");
    assert_eq!(ui.ev["reused_session"]["stage"], 0, "{}", ui.ev);
    assert_eq!(ui.ev["reused_session"]["name"], "产品需求");
    assert_eq!(ui.ev["reused_session"]["run"], run_id(&req));
    assert_eq!(ui.ev["reused_session"]["session"], "s-1");
    assert_eq!(ui.ev["reused_session"]["forked"], true);
    assert!(ui.ev["fresh_session_reason"].is_null(), "{}", ui.ev);
    assert_eq!(ui.ev["resume_context"], 9000);
    assert_eq!(ui.ev["resume_threshold"], aep::session_reuse::DEFAULT_LIMIT);
    // Short prompt: header, rules, reading rules, archive — no memory body or prior-run line.
    assert!(ui.prompt.starts_with("AEP Studio 项目："), "{}", ui.prompt);
    assert!(ui.prompt.contains("阶段：交互设计"), "{}", ui.prompt);
    assert!(ui.prompt.contains(READING));
    assert!(
        ui.prompt
            .contains(&format!("本次档案 {}", ui.dir.canonicalize()?.display()))
    );
    for absent in [
        "项目记忆",
        "仓库记忆",
        "前序执行 ",
        "## 上一轮答复",
        "OUT-1",
    ] {
        assert!(!ui.prompt.contains(absent), "{absent:?} in {}", ui.prompt);
    }
    assert_eq!(ui.ev["prompt_bytes"], ui.prompt.len());
    // AC-8: the window holds this stage's two calls, not the forked history's.
    assert_window(&ui, 9000);
    item.accept(1)?;

    // 技术方案: forks 交互设计's session (the chain follows the latest stage).
    let design = item.run(2, "")?;
    assert_minimal(&design);
    assert!(
        design.has(&["--resume", "s-2", "--fork-session"]),
        "{:?}",
        design.argv
    );
    assert_eq!(design.ev["session"], "s-3");
    assert_eq!(design.ev["reused_session"]["stage"], 1);
    assert_eq!(design.ev["reused_session"]["name"], "交互设计");
    assert_eq!(design.ev["reused_session"]["run"], run_id(&ui));
    assert_eq!(design.ev["reused_session"]["session"], "s-2");
    assert!(design.prompt.contains("阶段：技术方案"));
    assert!(!design.prompt.contains("前序执行 "), "{}", design.prompt);
    assert_window(&design, 9000);

    // AC-5: reuse changes nothing about the records — each stage has its own run, answer,
    // evidence and hand-off, and the earlier runs were not touched by later ones.
    let runs = [&req, &ui, &design];
    for (i, r) in runs.iter().enumerate() {
        let n = i + 1;
        assert_eq!(r.ev["run_id"], run_id(r));
        assert!(r.result().contains(&format!("OUT-{n}")), "{}", r.result());
        assert!(r.result().contains(&format!("AEP-HANDOFF: 结论 R{n}")));
        assert!(r.dir.join("stdout.log").is_file());
        let again: Value = serde_json::from_slice(&fs::read(r.dir.join("evidence.json"))?)?;
        assert_eq!(again["session"], format!("s-{n}"), "evidence kept: {again}");
    }
    assert_ne!(run_id(&req), run_id(&ui));
    assert_ne!(run_id(&ui), run_id(&design));
    // The accepted stages hand their own results forward.
    let p = item.lib.project().unwrap();
    let work = p.work.iter().find(|w| w.id == item.wid).unwrap();
    assert!(work.notes[0].contains("OUT-1"), "{:?}", work.notes);
    assert!(work.notes[1].contains("OUT-2"), "{:?}", work.notes);
    assert_eq!(work.current_attempt[&2], run_id(&design));
    Ok(())
}

#[test]
fn a_different_model_starts_a_new_session_with_the_full_prompt() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut item = Item::new(temp.path(), "diet-model")?;
    let req = item.run(0, "")?;
    item.accept(0)?;

    // AC-7: 交互设计 runs another model, so 产品需求's session is not continued.
    let ui = item.run(1, "sonnet")?;
    assert_minimal(&ui);
    assert!(!ui.argv.iter().any(|a| a == "--resume"), "{:?}", ui.argv);
    assert!(!ui.argv.iter().any(|a| a == "--fork-session"));
    assert!(ui.has(&["--model", "sonnet"]), "{:?}", ui.argv);
    assert_eq!(ui.ev["fresh_session_reason"], "model_differs", "{}", ui.ev);
    assert!(ui.ev["reused_session"].is_null(), "{}", ui.ev);
    assert!(
        ui.ev["resume_context"].is_null(),
        "no threshold check: {}",
        ui.ev
    );
    assert_eq!(ui.ev["session"], "s-2");
    // A new session gets the full prompt: memory body with the prior-run line.
    assert!(
        ui.prompt.contains(&format!("前序执行 {}", run_id(&req))),
        "{}",
        ui.prompt
    );
    assert!(ui.prompt.contains(READING));
    assert!(
        !ui.prompt.contains("## 上一轮答复"),
        "only for a session that exists"
    );
    assert_window(&ui, 9000);
    Ok(())
}

#[test]
fn a_session_over_the_resume_limit_is_replaced_by_a_digest() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut item = Item::new(temp.path(), "diet-limit")?;

    // 产品需求 ends with a 200K-token last call: over the default 150K limit.
    item.context(200_000)?;
    let req = item.run(0, "")?;
    assert_window(&req, 200_000);
    item.accept(0)?;

    // AC-4 (> limit): new session carrying the previous answer and hand-off digest.
    item.context(150_000)?;
    let ui = item.run(1, "")?;
    assert_minimal(&ui);
    assert!(!ui.argv.iter().any(|a| a == "--resume"), "{:?}", ui.argv);
    assert_eq!(
        ui.ev["fresh_session_reason"], "context_over_threshold",
        "{}",
        ui.ev
    );
    assert_eq!(ui.ev["resume_context"], 200_000);
    assert_eq!(ui.ev["resume_threshold"], 150_000);
    assert!(ui.ev["reused_session"].is_null(), "{}", ui.ev);
    assert_eq!(ui.ev["session"], "s-2");
    assert!(ui.prompt.contains(&format!("前序执行 {}", run_id(&req))));
    assert!(
        ui.prompt.contains("## 上一轮答复\n第 1 轮产出 OUT-1"),
        "{}",
        ui.prompt
    );
    assert!(
        ui.prompt
            .contains("## 交接摘要\n结论 R1；证据 specs/diet/；下一步 下一阶段"),
        "{}",
        ui.prompt
    );
    assert!(
        ui.prompt
            .contains(&req.dir.join("evidence.json").display().to_string())
    );
    assert!(ui.prompt.contains(READING));
    assert_window(&ui, 150_000);
    item.accept(1)?;

    // AC-4 (≤ limit): exactly at the limit the session is still continued.
    let design = item.run(2, "")?;
    assert!(
        design.has(&["--resume", "s-2", "--fork-session"]),
        "{:?}",
        design.argv
    );
    assert!(design.ev["fresh_session_reason"].is_null(), "{}", design.ev);
    assert_eq!(design.ev["resume_context"], 150_000);
    assert_eq!(design.ev["resume_threshold"], 150_000);
    assert_eq!(design.ev["reused_session"]["run"], run_id(&ui));
    assert!(
        !design.prompt.contains("## 上一轮答复"),
        "{}",
        design.prompt
    );
    Ok(())
}

#[test]
fn a_lower_project_resume_limit_starts_a_new_session_sooner() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut item = Item::new(temp.path(), "diet-project-limit")?;
    let home = item.home.clone();
    item.lib.change(&home, |lib| {
        lib.project_mut()?.resume_limit = Some(20_000);
        Ok(())
    })?;
    item.context(30_000)?;
    let req = item.run(0, "")?;
    let request: Value = serde_json::from_slice(&fs::read(req.dir.join("request.json"))?)?;
    assert_eq!(
        request["resume_limit"], 20_000,
        "reserve writes the project setting"
    );
    item.accept(0)?;
    let ui = item.run(1, "")?;
    assert!(!ui.argv.iter().any(|a| a == "--resume"), "{:?}", ui.argv);
    assert_eq!(ui.ev["fresh_session_reason"], "context_over_threshold");
    assert_eq!(ui.ev["resume_context"], 30_000);
    assert_eq!(ui.ev["resume_threshold"], 20_000);
    Ok(())
}
