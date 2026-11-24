//! End to end over the command layer: a real repository, a scripted Claude, the real runner.
use aep_studio_app::api;
use anyhow::{Result, ensure};
use serde_json::Value;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()?;
    ensure!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(String::from_utf8(out.stdout)?)
}

/// Answers per stage; the build writes code, every stage passes.
fn fake_claude(dir: &Path) -> Result<String> {
    let bin = dir.join("claude");
    fs::write(
        &bin,
        r#"#!/bin/sh
input=$(cat)
stage=$(printf '%s' "$input" | sed -n 's/^阶段：\([^（]*\)（.*/\1/p' | head -1)
[ -n "$stage" ] || stage=继续
case "$stage" in
  产品需求) mkdir -p specs/demo; echo prd > specs/demo/spec.md ;;
  开发实现) mkdir -p src; echo "fn main() {}" > src/main.rs ;;
esac
echo '{"type":"system","subtype":"init","session_id":"s-1","model":"sonnet"}'
echo '{"type":"assistant","message":{"content":[{"type":"text","text":"正在处理"}]}}'
printf '%s 完成\nAEP: PASS' "$stage" | python3 -c 'import json,sys; print(json.dumps({"type":"result","result":sys.stdin.read(),"is_error":False,"session_id":"s-1"}))'
"#,
    )?;
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o700))?;
    Ok(bin.display().to_string())
}

fn wait_idle(home: &Path, pid: &str, wid: &str) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let d = api::work_detail(home, pid, wid)?;
        if d["running"].is_null() && d["autopilot"]["running"] == false {
            return Ok(d);
        }
        ensure!(Instant::now() < deadline, "timed out: {d}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn a_work_item_runs_chats_advances_delivers_merges_and_archives() -> Result<()> {
    let runner = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/aep");
    ensure!(
        runner.is_file(),
        "build the runner first: cargo build --bin aep"
    );
    let t = tempfile::tempdir()?;
    let home = t.path().join("catalog");
    let repo = t.path().join("repo");
    fs::create_dir_all(&repo)?;
    git(&repo, &["init", "-q", "-b", "main"])?;
    git(&repo, &["config", "user.name", "t"])?;
    git(&repo, &["config", "user.email", "t@example.com"])?;
    fs::write(repo.join("README.md"), "demo\n")?;
    git(&repo, &["add", "."])?;
    git(&repo, &["commit", "-q", "-m", "init"])?;
    let claude = fake_claude(t.path())?;

    api::create_project(&home, "demo", &[repo.display().to_string()])?;
    let snap = api::snapshot(&home)?;
    let pid = snap["active"].as_str().unwrap().to_owned();
    let root = snap["projects"][0]["repositories"][0]
        .as_str()
        .unwrap()
        .to_owned();
    let wid = api::create_work(
        &home,
        &pid,
        "导出",
        "Software",
        false,
        std::slice::from_ref(&root),
    )?;
    let profile: aep::workflow::Profile = serde_json::from_value(serde_json::json!({
        "client": "Claude", "executable": claude, "model": "", "check_command": ""
    }))?;
    for i in 0..7 {
        api::update_stage(&home, &pid, &wid, i, None, Some(profile.clone()), None)?;
    }
    api::update_stage(
        &home,
        &pid,
        &wid,
        0,
        Some("支持导出".into()),
        None,
        Some("Network".into()),
    )?;

    // Run the first stage and read it back as a transcript.
    let attempt = api::run_stage(&home, &pid, &wid, 0, None)?;
    let d = wait_idle(&home, &pid, &wid)?;
    let s0 = &d["stages"][0];
    assert_eq!(s0["sandbox"], "Network");
    assert_eq!(s0["attempts"][0]["status"], "succeeded", "{d}");
    assert_eq!(s0["attempts"][0]["prompt"], "支持导出");
    assert_eq!(s0["attempts"][0]["committed"], true);
    let view = api::attempt_view(&home, &pid, &wid, &attempt)?;
    assert!(
        view["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "message"),
        "{view}"
    );
    assert!(view["result"].as_str().unwrap().contains("AEP: PASS"));

    // Keep chatting in the same stage, then confirm it.
    api::follow_up(&home, &pid, &wid, 0, "补充边界情况")?;
    let d = wait_idle(&home, &pid, &wid)?;
    let turns = d["stages"][0]["attempts"].as_array().unwrap();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[1]["follow_up"], true);
    assert_eq!(turns[1]["prompt"], "补充边界情况");
    api::advance(&home, &pid, &wid, 0)?;

    // Skip the optional design stage ahead of time, then autopilot to delivery.
    api::skip(&home, &pid, &wid, 1, "无界面改动")?;
    api::autopilot_start(&home, &pid, &wid, 6)?;
    let d = wait_idle(&home, &pid, &wid)?;
    let log = fs::read_to_string(
        home.join("workflows")
            .join(&pid)
            .join(&wid)
            .join("autopilot.log"),
    )
    .unwrap_or_default();
    assert_eq!(
        d["autopilot"]["status"], "done",
        "{} log: {log}",
        d["autopilot"]
    );
    assert_eq!(d["delivered"], true);
    assert_eq!(d["stages"][1]["state"], "skipped");
    assert!(d["branch"].as_str().unwrap().starts_with("aep-stage/"));

    // The delivery merges the commit independent acceptance approved.
    let preview = api::merge_preview(&home, &pid, &wid, None)?;
    assert!(preview["error"].is_null(), "{preview}");
    assert_eq!(preview["target"], "main");
    assert!(preview["diff"].as_str().unwrap().contains("src/main.rs"));
    let before = api::work_detail(&home, &pid, &wid)?;
    assert_eq!(
        before["branches"][0]["merged"], false,
        "{}",
        before["branches"]
    );
    api::merge_confirm(
        &home,
        &pid,
        &wid,
        "main",
        preview["digest"].as_str().unwrap(),
    )?;
    assert!(git(&repo, &["show", "main:src/main.rs"])?.contains("fn main"));
    let d = api::work_detail(&home, &pid, &wid)?;
    assert_eq!(d["deliveries"][0]["merged"]["target"], "main");

    assert_eq!(d["branches"][0]["merged"], true, "{}", d["branches"]);

    // An unmerged restart branch of the same item survives a normal cleanup.
    let extra = format!("aep-stage/{wid}-restart");
    let stray = git(
        &repo,
        &["commit-tree", "-p", "main", "-m", "unmerged", "main^{tree}"],
    )?;
    git(&repo, &["branch", &extra, stray.trim()])?;

    // Archive: memory written, worktree and merged branch removed, unmerged branch kept.
    let draft = d["memory_draft"].as_str().unwrap().to_owned();
    assert!(draft.contains("「导出」已交付"));
    api::archive(&home, &pid, &wid, &draft, true)?;
    let d = api::work_detail(&home, &pid, &wid)?;
    assert!(d["shelved"]["memory"].is_i64());
    assert!(d["worktrees"].as_array().unwrap().is_empty());
    let left = git(&repo, &["branch", "--list", "aep-stage/*"])?;
    assert_eq!(left.trim(), extra, "{left}");
    assert_eq!(
        d["branches"].as_array().unwrap().len(),
        1,
        "{}",
        d["branches"]
    );
    assert_eq!(d["branches"][0]["merged"], false);
    // Forcing removes it too.
    let cleaned = api::cleanup(&home, &pid, &wid, true)?;
    assert_eq!(cleaned.deleted, vec![extra.clone()]);
    assert!(
        git(&repo, &["branch", "--list", "aep-stage/*"])?
            .trim()
            .is_empty()
    );
    let notes = api::memories(&home, &pid)?;
    assert!(notes.as_array().unwrap().iter().any(|n| n["body"] == draft));
    assert_eq!(
        api::snapshot(&home)?["projects"][0]["work"][0]["group"],
        "shelved"
    );

    // History lists every run with what it wrote to memory, and the merge.
    let h = api::history(&home, &pid, &wid)?;
    let rounds = h["rounds"].as_array().unwrap();
    assert!(rounds.len() >= 6, "{h}");
    assert_eq!(rounds[0]["stage_name"], "产品需求");
    assert!(
        rounds.iter().any(|r| r["agents"][0]["writes"]
            .as_array()
            .is_some_and(|w| !w.is_empty())),
        "{h}"
    );
    assert_eq!(h["merges"][0]["target"], "main");

    // Items hand notes to each other.
    let other = api::create_work(&home, &pid, "对账", "Software", false, &[])?;
    api::send_handoff(&home, &pid, &wid, &other, "导出格式已定：UTF-8 BOM")?;
    let got = api::handoffs(&home, &pid, &other)?;
    assert_eq!(got[0]["from"], wid.as_str());
    assert_eq!(got[0]["note"], "导出格式已定：UTF-8 BOM");
    assert!(api::send_handoff(&home, &pid, &wid, &wid, "x").is_err());

    // An item that never ran can be deleted; one that ran cannot.
    api::delete_work(&home, &pid, &other)?;
    assert!(api::work_detail(&home, &pid, &other).is_err());
    assert!(api::delete_work(&home, &pid, &wid).is_err());

    // Repository memory, which stages also read.
    let id = api::add_repo_memory(&root, "金额统一用分存储")?;
    let notes = api::repo_memories(&root)?;
    assert_eq!(notes[0]["id"], id);
    assert_eq!(notes[0]["body"], "金额统一用分存储");
    Ok(())
}

#[test]
fn a_failed_gate_sends_the_item_back_and_starts_the_fix() -> Result<()> {
    let t = tempfile::tempdir()?;
    let home = t.path().join("catalog");
    let bin = t.path().join("claude");
    fs::write(
        &bin,
        r#"#!/bin/sh
input=$(cat)
stage=$(printf '%s' "$input" | sed -n 's/^阶段：\([^（]*\)（.*/\1/p' | head -1)
answer="完成
AEP: PASS"
[ "$stage" = "测试验证" ] && answer="缺少表头
AEP: FAIL"
printf '%s' "$answer" | python3 -c 'import json,sys; print(json.dumps({"type":"result","result":sys.stdin.read(),"is_error":False,"session_id":"s"}))'
"#,
    )?;
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o700))?;
    api::create_project(&home, "p", &[])?;
    let pid = api::snapshot(&home)?["active"].as_str().unwrap().to_owned();
    let wid = api::create_work(&home, &pid, "导出", "Software", false, &[])?;
    let profile: aep::workflow::Profile = serde_json::from_value(serde_json::json!({
        "client": "Claude", "executable": bin.display().to_string(), "model": "", "check_command": ""
    }))?;
    for i in 0..7 {
        api::update_stage(&home, &pid, &wid, i, None, Some(profile.clone()), None)?;
    }
    for i in 0..4 {
        api::run_stage(&home, &pid, &wid, i, None)?;
        wait_idle(&home, &pid, &wid)?;
        api::advance(&home, &pid, &wid, i)?;
    }
    api::run_stage(&home, &pid, &wid, 4, None)?;
    let d = wait_idle(&home, &pid, &wid)?;
    assert_eq!(d["stages"][4]["attempts"][0]["status"], "failed");
    let fix = api::send_back(&home, &pid, &wid, 4, 3, None)?;
    let d = wait_idle(&home, &pid, &wid)?;
    assert_eq!(d["stage"], 3);
    assert_eq!(d["reworks"][0]["from"], 4);
    let last = d["stages"][3]["attempts"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(last["id"], fix);
    assert!(
        last["prompt"]
            .as_str()
            .unwrap()
            .contains("「测试验证」未通过"),
        "{last}"
    );
    // The fixing stage gets the reviewer's full report inline, not just a pointer to it.
    let prompt = last["prompt"].as_str().unwrap();
    assert!(
        prompt.contains("缺少表头") && prompt.contains("AEP: FAIL"),
        "{prompt}"
    );

    // Back at the test stage after the fix: it re-checks in its own session, told why.
    api::advance(&home, &pid, &wid, 3)?;
    let recheck = api::run_stage(&home, &pid, &wid, 4, Some("重点看空账户".into()))?;
    let d = wait_idle(&home, &pid, &wid)?;
    let turn = d["stages"][4]["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == recheck.as_str())
        .unwrap()
        .clone();
    assert_eq!(turn["follow_up"], true, "{turn}");
    let p = turn["prompt"].as_str().unwrap();
    assert!(p.contains("复核你上次指出的问题"), "{p}");
    assert!(
        p.contains("「测试验证」→「开发实现」") && p.contains("缺少表头"),
        "{p}"
    );
    assert!(p.contains("「开发实现」修改后的交接"), "{p}");
    assert!(p.contains("补充说明：重点看空账户"), "{p}");
    Ok(())
}

#[test]
fn a_new_project_imports_claude_memory_and_existing_branches() -> Result<()> {
    let t = tempfile::tempdir()?;
    let home = t.path().join("catalog");
    let repo = t.path().join("repo");
    fs::create_dir_all(&repo)?;
    git(&repo, &["init", "-q", "-b", "main"])?;
    git(&repo, &["config", "user.name", "t"])?;
    git(&repo, &["config", "user.email", "t@example.com"])?;
    fs::write(repo.join("README.md"), "demo\n")?;
    fs::write(repo.join("CLAUDE.md"), "rules\n")?;
    git(&repo, &["add", "."])?;
    git(&repo, &["commit", "-q", "-m", "init"])?;
    // An agent's branch in its own worktree: one commit ahead, one uncommitted file.
    let wt = t.path().join("wt");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feat/export",
            wt.to_str().unwrap(),
        ],
    )?;
    fs::write(wt.join("export.rs"), "// wip\n")?;
    git(&wt, &["add", "."])?;
    git(&wt, &["commit", "-q", "-m", "start export"])?;
    fs::write(wt.join("notes.txt"), "dirty\n")?;
    let tip = git(&repo, &["rev-parse", "feat/export"])?.trim().to_owned();

    // What Claude Code and Codex keep about this folder.
    let root = repo.canonicalize()?;
    let slug: String = root
        .to_string_lossy()
        .chars()
        .map(|c| if matches!(c, '/' | '.') { '-' } else { c })
        .collect();
    let claude = t.path().join("claude-home").join("projects").join(&slug);
    fs::create_dir_all(claude.join("memory"))?;
    fs::write(claude.join("memory/MEMORY.md"), "- index\n")?;
    fs::write(
        claude.join("memory/money.md"),
        "---\nname: money-in-cents\ndescription: 金额统一用分\nmetadata:\n  type: project\n---\n\n所有金额字段用整数分存储。\n",
    )?;
    fs::write(claude.join("s1.jsonl"), "{}\n")?;
    let codex = t.path().join("codex/sessions/2026/11");
    fs::create_dir_all(&codex)?;
    fs::write(
        codex.join("rollout.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({"type":"session_meta","payload":{"cwd": root}})
        ),
    )?;
    // SAFETY: only this test reads these variables.
    unsafe {
        std::env::set_var("CLAUDE_CONFIG_DIR", t.path().join("claude-home"));
        std::env::set_var("CODEX_HOME", t.path().join("codex"));
    }

    let found = serde_json::to_value(api::import_scan(&repo.display().to_string())?)?;
    assert_eq!(found["memories"][0]["file"], "money.md", "{found}");
    assert_eq!(found["memories"][0]["title"], "money-in-cents");
    assert_eq!(found["memories"].as_array().unwrap().len(), 1);
    assert_eq!(found["rules"], serde_json::json!(["CLAUDE.md"]));
    assert_eq!(found["claude_sessions"], 1);
    assert_eq!(found["codex_sessions"], 1);
    let w = &found["worktrees"][0];
    assert_eq!(w["branch"], "feat/export", "{found}");
    assert_eq!(
        (
            w["ahead"].as_u64(),
            w["dirty"].as_u64(),
            w["untracked"].as_u64()
        ),
        (Some(1), Some(0), Some(1))
    );
    assert_eq!(w["source"], "Git");

    let pid = api::create_project(&home, "demo", &[repo.display().to_string()])?;
    let wt_path = w["path"].as_str().unwrap().to_owned();
    let done = api::import_apply(
        &home,
        &pid,
        &repo.display().to_string(),
        &["money.md".into()],
        &[("feat/export".into(), wt_path.clone())],
    )?;
    assert_eq!(done["notes"], 1, "{done}");
    let wid = done["items"][0].as_str().unwrap().to_owned();
    let notes = api::memories(&home, &pid)?;
    assert!(
        notes
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["body"].as_str().is_some_and(|b| b
                .contains("导入自 Claude 记忆：money-in-cents")
                && b.contains("整数分"))),
        "{notes}"
    );
    // Importing the same memory again adds nothing.
    let again = api::import_apply(
        &home,
        &pid,
        &repo.display().to_string(),
        &["money.md".into()],
        &[],
    )?;
    assert_eq!(again["notes"], 0);
    // A changed file becomes a new version of its note; agents see only the newest.
    fs::write(
        claude.join("memory/money.md"),
        "---\nname: money-in-cents\ndescription: 金额统一用分\n---\n\n所有金额字段用整数分存储，展示时再换算。\n",
    )?;
    let changed = api::import_apply(
        &home,
        &pid,
        &repo.display().to_string(),
        &["money.md".into()],
        &[],
    )?;
    assert_eq!(changed["notes"], 1);
    let notes = api::memories(&home, &pid)?;
    let notes = notes.as_array().unwrap();
    let current: Vec<_> = notes.iter().filter(|n| n["superseded"] == false).collect();
    assert_eq!(current.len(), 1, "{notes:?}");
    assert!(
        current[0]["body"]
            .as_str()
            .unwrap()
            .contains("展示时再换算")
    );
    let first = current[0]["replaces"]
        .as_i64()
        .expect("new version replaces the old");
    // Editing makes another version; restoring the first is revising with its text.
    let edited = api::revise_memory(
        &home,
        &pid,
        current[0]["id"].as_i64().unwrap(),
        "改用 Decimal",
    )?;
    let original = notes.iter().find(|n| n["id"] == first).unwrap()["body"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        api::revise_memory(&home, &pid, first, "x").is_err(),
        "only the newest can be revised"
    );
    let restored = api::revise_memory(&home, &pid, edited, &original)?;
    let notes = api::memories(&home, &pid)?;
    let newest = notes
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == restored)
        .unwrap();
    assert_eq!(newest["body"], original.as_str());
    assert_eq!(newest["replaces"], edited);

    // The branch became an item at 开发实现 with its own branch at the same commit.
    let d = api::work_detail(&home, &pid, &wid)?;
    assert_eq!(d["name"], "feat/export");
    assert_eq!(d["stage"], 3);
    for i in 0..3 {
        assert_eq!(d["stages"][i]["state"], "skipped", "{}", d["stages"][i]);
    }
    assert_eq!(
        git(&repo, &["rev-parse", &format!("aep-stage/{wid}")])?.trim(),
        tip
    );

    // Its build runs in a fresh worktree on that branch; the original worktree is untouched.
    let profile: aep::workflow::Profile = serde_json::from_value(serde_json::json!({
        "client": "Claude", "executable": fake_claude(t.path())?, "model": "", "check_command": ""
    }))?;
    api::update_stage(&home, &pid, &wid, 3, None, Some(profile), None)?;
    let run = api::run_stage(&home, &pid, &wid, 3, None)?;
    let d = wait_idle(&home, &pid, &wid)?;
    let turn = &d["stages"][3]["attempts"][0];
    assert_eq!(turn["status"], "succeeded", "{d}");
    assert!(
        turn["prompt"]
            .as_str()
            .unwrap()
            .contains("接手已有分支 `feat/export`")
    );
    let view = api::attempt_view(&home, &pid, &wid, &run)?;
    let workspace = Path::new(view["workspace"].as_str().unwrap());
    assert!(workspace.join("export.rs").is_file());
    assert!(workspace.join("src/main.rs").is_file());
    assert!(
        git(&repo, &["log", "--format=%s", &format!("aep-stage/{wid}")])?.contains("start export")
    );
    assert!(Path::new(&wt_path).join("notes.txt").is_file());
    assert_eq!(git(&repo, &["rev-parse", "feat/export"])?.trim(), tip);

    // The branch page: every branch with its worktree, item branches named by their item.
    let g = api::branch_graph(&home, &pid, &repo.display().to_string(), 200)?;
    let branches = g["branches"].as_array().unwrap();
    let feat = branches
        .iter()
        .find(|b| b["name"] == "feat/export")
        .unwrap();
    assert_eq!(feat["ahead"], 1, "{feat}");
    assert_eq!(feat["worktree"]["source"], "Git");
    assert_eq!(feat["worktree"]["untracked"], 1);
    let ours = branches
        .iter()
        .find(|b| b["name"] == format!("aep-stage/{wid}").as_str())
        .unwrap();
    assert_eq!(ours["item"]["name"], "feat/export", "{ours}");
    assert_eq!(ours["worktree"]["source"], "AEP");
    assert!(
        branches
            .iter()
            .any(|b| b["name"] == "main" && b["current"] == true)
    );
    let subjects: Vec<_> = g["commits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["subject"].as_str().unwrap())
        .collect();
    assert!(
        subjects.contains(&"start export") && subjects.contains(&"init"),
        "{subjects:?}"
    );

    // A git tool plugin from the config file opens the chosen folder.
    fs::create_dir_all(home.join("plugins"))?;
    fs::write(
        home.join("plugins/git-tools.json"),
        r#"[{"label":"标记","command":["/usr/bin/touch","{path}/opened"]}]"#,
    )?;
    let tools = api::git_tools(&home)?;
    let tool = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["label"] == "标记")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    api::open_git_tool(&home, &tool, &wt_path)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !Path::new(&wt_path).join("opened").exists() {
        ensure!(Instant::now() < deadline, "tool did not run");
        std::thread::sleep(Duration::from_millis(50));
    }

    // Importing the same branch again gives back the same item instead of a copy.
    let again = api::import_apply(
        &home,
        &pid,
        &repo.display().to_string(),
        &[],
        &[("feat/export".into(), wt_path.clone())],
    )?;
    assert_eq!(again["items"][0], wid.as_str());

    // Sync: a new worktree branch appears, then its worktree and a memory file go away.
    let wt2 = t.path().join("wt2");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feat/b",
            wt2.to_str().unwrap(),
        ],
    )?;
    // Nothing on it yet (and ZCode's own folder is not a change): not an item.
    fs::create_dir_all(wt2.join(".zcode"))?;
    fs::write(wt2.join(".zcode/state.json"), "{}")?;
    let plan = serde_json::to_value(api::import_sync_plan(
        &home,
        &pid,
        &repo.display().to_string(),
    )?)?;
    assert!(plan["items_add"].as_array().unwrap().is_empty(), "{plan}");
    assert_eq!(plan["items_skip"][0]["branch"], "feat/b", "{plan}");
    fs::write(wt2.join("b.txt"), "b\n")?;
    git(&wt2, &["add", "b.txt"])?;
    git(&wt2, &["commit", "-q", "-m", "b"])?;
    let plan = serde_json::to_value(api::import_sync_plan(
        &home,
        &pid,
        &repo.display().to_string(),
    )?)?;
    assert_eq!(plan["items_add"][0]["branch"], "feat/b", "{plan}");
    assert_eq!(plan["items_keep"][0]["id"], wid.as_str());
    // The note was restored to an older text above, so sync brings it back to the file.
    assert_eq!(plan["memories_update"], serde_json::json!(["money.md"]));
    api::import_sync(&home, &pid, &repo.display().to_string(), true, true)?;
    let synced = api::snapshot(&home)?["projects"][0]["work"].clone();
    let b = synced
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == "feat/b")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // Deleting the imported item is remembered: sync does not bring it back.
    api::delete_work(&home, &pid, &b)?;
    let plan = serde_json::to_value(api::import_sync_plan(
        &home,
        &pid,
        &repo.display().to_string(),
    )?)?;
    assert!(plan["items_add"].as_array().unwrap().is_empty(), "{plan}");
    assert_eq!(plan["items_skip"][0]["reason"], "你删除过这个导入的事项");
    // Importing it on purpose brings it back.
    let back = api::import_apply(
        &home,
        &pid,
        &repo.display().to_string(),
        &[],
        &[("feat/b".into(), wt2.display().to_string())],
    )?;
    let b = back["items"][0].as_str().unwrap().to_owned();
    git(
        &repo,
        &["worktree", "remove", "--force", wt2.to_str().unwrap()],
    )?;
    fs::remove_file(claude.join("memory/money.md"))?;
    let plan = serde_json::to_value(api::import_sync_plan(
        &home,
        &pid,
        &repo.display().to_string(),
    )?)?;
    assert_eq!(plan["items_remove"][0]["id"], b.as_str(), "{plan}");
    assert_eq!(plan["memories_remove"][0]["title"], "money-in-cents");
    assert!(plan["items_add"].as_array().unwrap().is_empty());
    api::import_sync(&home, &pid, &repo.display().to_string(), true, true)?;
    assert!(api::work_detail(&home, &pid, &b).is_err());
    assert!(
        git(&repo, &["branch", "--list", &format!("aep-stage/{b}")])?
            .trim()
            .is_empty()
    );
    assert!(
        api::work_detail(&home, &pid, &wid).is_ok(),
        "the item that ran stays"
    );
    let notes = api::memories(&home, &pid)?;
    assert!(
        !notes
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["superseded"] == false
                && n["body"]
                    .as_str()
                    .unwrap_or("")
                    .contains("导入自 Claude 记忆")),
        "{notes}"
    );

    // The project can be taken out of Studio; its records are set aside, the repo stays.
    api::remove_project(&home, &pid)?;
    assert!(
        api::snapshot(&home)?["projects"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(!home.join("workflows").join(&pid).exists());
    assert_eq!(fs::read_dir(home.join("workflows/.removed"))?.count(), 1);
    assert!(repo.join("README.md").is_file());
    Ok(())
}

#[test]
fn memory_is_tidied_by_a_cli_and_applied_by_choice() -> Result<()> {
    let t = tempfile::tempdir()?;
    let home = t.path().join("catalog");
    let pid = api::create_project(&home, "tidy", &[])?;
    let a = api::add_memory(&home, &pid, "金额用分")?;
    let b = api::add_memory(&home, &pid, "金额统一用分存储")?;
    let c = api::add_memory(&home, &pid, "旧部署流程：手动 scp")?;
    // A scripted Claude that proposes merging the two money notes and retiring the old one.
    let bin = t.path().join("bin");
    fs::create_dir_all(&bin)?;
    fs::write(
        bin.join("claude"),
        format!(
            r#"#!/bin/sh
input=$(cat)
pwd > "{dir}/cwd-seen"
if printf '%s' "$input" | grep -q '整理成开发事项'; then
  printf '%s' "$input" > "{dir}/draft-seen"
  answer='{{"summary":"拆成两件","items":[{{"name":"导出 · CSV","template":"Software","brief":"导出对账单为 CSV"}},{{"name":"导出 · 表头","brief":"空账户也导出表头"}}]}}'
else
  printf '%s' "$input" > "{dir}/prompt-seen"
  answer='{{"summary":"合并重复","ops":[{{"op":"merge","ids":[{a},{b}],"text":"金额统一用整数分存储","why":"重复"}},{{"op":"retire","id":{c},"why":"已改为自动部署"}}]}}'
fi
printf '%s' "$answer" | python3 -c 'import json,sys; print(json.dumps({{"type":"result","result":sys.stdin.read(),"is_error":False}}))'
"#,
            dir = t.path().display()
        ),
    )?;
    fs::set_permissions(bin.join("claude"), fs::Permissions::from_mode(0o700))?;
    // SAFETY: only adds a directory in front of PATH; other tests use explicit executables.
    unsafe {
        std::env::set_var(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
    };

    let job = api::tidy_start(&home, &pid, "Claude", "", "合并重复的")?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        let s = api::tidy_status(&home, &pid)?;
        if s["status"] == "succeeded" || s["status"] == "failed" {
            break s;
        }
        ensure!(Instant::now() < deadline, "tidy did not finish: {s}");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(status["status"], "succeeded", "{status}");
    assert_eq!(status["id"], job.as_str());
    assert_eq!(status["proposal"]["ops"].as_array().unwrap().len(), 2);
    let seen = fs::read_to_string(t.path().join("prompt-seen"))?;
    assert!(
        seen.contains("金额用分") && seen.contains("整理要求：合并重复的"),
        "{seen}"
    );
    // Nothing changed yet; accept only the merge.
    assert_eq!(api::tidy_apply(&home, &pid, &job, &[0])?, 1);
    let notes = api::memories(&home, &pid)?;
    let current: Vec<&str> = notes
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["superseded"] == false)
        .map(|n| n["body"].as_str().unwrap())
        .collect();
    assert_eq!(current.len(), 2, "{current:?}");
    assert!(current.contains(&"金额统一用整数分存储") && current.contains(&"旧部署流程：手动 scp"));
    // Re-applying the merge is refused: its notes are no longer current.
    assert!(api::tidy_apply(&home, &pid, &job, &[0]).is_err());

    // Drafting items: the CLI works in the repository and sees memory and existing items.
    let repo = t.path().join("repo");
    fs::create_dir_all(&repo)?;
    git(&repo, &["init", "-q", "-b", "main"])?;
    api::add_repository(&home, &pid, &repo.display().to_string())?;
    api::create_work(&home, &pid, "已有的事", "Software", false, &[])?;
    let draft = api::draft_start(&home, &pid, "Claude", "", "支持导出对账单", None)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        let s = api::draft_status(&home, &pid)?;
        if s["status"] == "succeeded" || s["status"] == "failed" {
            break s;
        }
        ensure!(Instant::now() < deadline, "draft did not finish: {s}");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(status["status"], "succeeded", "{status}");
    assert_eq!(status["id"], draft.as_str());
    assert_eq!(status["draft"]["items"][1]["name"], "导出 · 表头");
    assert_eq!(status["draft"]["items"][1]["template"], "Software");
    let seen = fs::read_to_string(t.path().join("draft-seen"))?;
    assert!(
        seen.contains("支持导出对账单")
            && seen.contains("金额统一用整数分存储")
            && seen.contains("- 已有的事"),
        "{seen}"
    );
    assert_eq!(
        Path::new(fs::read_to_string(t.path().join("cwd-seen"))?.trim()).canonicalize()?,
        repo.canonicalize()?
    );
    Ok(())
}

#[test]
fn a_stage_is_stopped_at_its_token_budget_and_continues_after_more_is_added() -> Result<()> {
    let runner = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/aep");
    ensure!(
        runner.is_file(),
        "build the runner first: cargo build --bin aep"
    );
    let t = tempfile::tempdir()?;
    let home = t.path().join("catalog");
    let repo = t.path().join("repo");
    fs::create_dir_all(&repo)?;
    git(&repo, &["init", "-q", "-b", "main"])?;
    git(&repo, &["config", "user.name", "t"])?;
    git(&repo, &["config", "user.email", "t@example.com"])?;
    fs::write(repo.join("README.md"), "demo\n")?;
    git(&repo, &["add", "."])?;
    git(&repo, &["commit", "-q", "-m", "init"])?;
    // Spends 6000 tokens per model call, one call a second, and never finishes on its own
    // unless resumed.
    let bin = t.path().join("claude");
    fs::write(
        &bin,
        r#"#!/bin/sh
cat > /dev/null
echo '{"type":"system","subtype":"init","session_id":"s-budget","model":"sonnet"}'
case "$*" in
  *--resume*)
    echo '{"type":"assistant","message":{"id":"m9","usage":{"input_tokens":10,"cache_read_input_tokens":100,"cache_creation_input_tokens":0,"output_tokens":5},"content":[{"type":"text","text":"继续完成"}]}}'
    printf '完成\nAEP: PASS' | python3 -c 'import json,sys; print(json.dumps({"type":"result","result":sys.stdin.read(),"is_error":False,"session_id":"s-budget","usage":{"input_tokens":10,"cache_read_input_tokens":100,"cache_creation_input_tokens":0,"output_tokens":5}}))'
    exit 0 ;;
esac
i=0
while [ $i -lt 30 ]; do
  i=$((i+1))
  echo "{\"type\":\"assistant\",\"message\":{\"id\":\"m$i\",\"usage\":{\"input_tokens\":1000,\"cache_read_input_tokens\":5000,\"cache_creation_input_tokens\":0,\"output_tokens\":0},\"content\":[{\"type\":\"tool_use\",\"id\":\"t$i\",\"name\":\"Read\",\"input\":{\"file_path\":\"README.md\"}}]}}"
  sleep 1
done
"#,
    )?;
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o700))?;

    api::create_project(&home, "demo", &[repo.display().to_string()])?;
    let snap = api::snapshot(&home)?;
    let pid = snap["active"].as_str().unwrap().to_owned();
    let root = snap["projects"][0]["repositories"][0]
        .as_str()
        .unwrap()
        .to_owned();
    let wid = api::create_work(
        &home,
        &pid,
        "预算",
        "Software",
        false,
        std::slice::from_ref(&root),
    )?;
    let profile: aep::workflow::Profile = serde_json::from_value(serde_json::json!({
        "client": "Claude", "executable": bin.display().to_string(), "model": "", "check_command": ""
    }))?;
    api::update_stage(
        &home,
        &pid,
        &wid,
        0,
        Some("做点事".into()),
        Some(profile),
        None,
    )?;
    api::set_budget(&home, &pid, &wid, Some(0), Some(15_000))?;
    // A worktree made before stage worktrees moved out of `.git` is moved on the next run.
    let old = repo.join(".git/aep/workflow-worktrees").join(&wid);
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &format!("aep-stage/{wid}"),
            old.to_str().unwrap(),
        ],
    )?;
    fs::write(old.join("draft.md"), "unsaved\n")?;

    let first = api::run_stage(&home, &pid, &wid, 0, None)?;
    let d = wait_idle(&home, &pid, &wid)?;
    let a = &d["stages"][0]["attempts"][0];
    assert_eq!(a["status"], "budget_exhausted", "{d}");
    assert!(a["error"].as_str().unwrap().contains("Token 预算已用完"));
    let tree = std::path::PathBuf::from(d["worktrees"][0].as_str().unwrap());
    assert!(tree.starts_with(home.canonicalize()?.join("worktrees")), "{tree:?}");
    assert!(!old.exists());
    assert_eq!(fs::read_to_string(tree.join("draft.md"))?, "unsaved\n");
    let used = d["stages"][0]["budget"]["used"].as_u64().unwrap();
    assert!(
        (18_000..40_000).contains(&used),
        "stopped soon after the limit: {used}"
    );
    // Nothing more runs until budget is added.
    assert!(api::run_stage(&home, &pid, &wid, 0, None).is_err());
    let view = api::attempt_view(&home, &pid, &wid, &first)?;
    assert_eq!(view["token_limit"], 15_000);
    let analysis = api::usage_analysis(&home, &pid, &wid, &first)?;
    assert_eq!(analysis["by_kind"][0]["kind"], "读取");

    let next = api::extend_budget(&home, &pid, &wid, 0, 50_000, "")?;
    let d = wait_idle(&home, &pid, &wid)?;
    let a = &d["stages"][0]["attempts"][1];
    assert_eq!(a["id"], next);
    assert_eq!(a["status"], "succeeded", "{d}");
    assert_eq!(a["follow_up"], true);
    assert!(d["stages"][0]["budget"]["limit"].as_u64().unwrap() >= used + 50_000);

    let usage = api::project_usage(&home, &pid)?;
    assert_eq!(usage["total"]["runs"], 2);
    assert_eq!(usage["stages"][0]["key"], "产品需求");
    Ok(())
}
