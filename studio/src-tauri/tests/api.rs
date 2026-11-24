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
        (w["ahead"].as_u64(), w["dirty"].as_u64()),
        (Some(1), Some(1))
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
