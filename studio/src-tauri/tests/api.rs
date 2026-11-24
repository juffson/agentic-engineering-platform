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

    // Archive: memory written, worktree removed, branch kept.
    let draft = d["memory_draft"].as_str().unwrap().to_owned();
    assert!(draft.contains("「导出」已交付"));
    api::archive(&home, &pid, &wid, &draft, true)?;
    let d = api::work_detail(&home, &pid, &wid)?;
    assert!(d["shelved"]["memory"].is_i64());
    assert!(d["worktrees"].as_array().unwrap().is_empty());
    assert!(git(&repo, &["branch", "--list", "aep-stage/*"])?.contains("aep-stage/"));
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
