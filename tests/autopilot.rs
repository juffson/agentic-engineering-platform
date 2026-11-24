use aep::{
    autopilot,
    project::{Kind, Library},
    workflow::{Client, Profile, Template},
};
use anyhow::{Result, ensure};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

/// A Claude stand-in whose answer per stage comes from `<dir>/<stage>.<visit>` files,
/// defaulting to a passing answer.
fn fake(dir: &Path) -> Result<Profile> {
    let bin = dir.join("claude");
    fs::write(
        &bin,
        format!(
            r#"#!/bin/sh
input=$(cat)
stage=$(printf '%s' "$input" | sed -n 's/^阶段：\([^（]*\)（.*/\1/p' | head -1)
[ -n "$stage" ] || stage=continue
n=$(cat "{d}/$stage.count" 2>/dev/null || echo 0); n=$((n+1)); echo $n > "{d}/$stage.count"
printf '%s\n' "$input" > "{d}/$stage.$n.prompt"
answer="$stage 完成
AEP: PASS"
[ -f "{d}/$stage.$n" ] && answer=$(cat "{d}/$stage.$n")
if [ -f "{d}/$stage.$n.crash" ]; then
  echo '{{"type":"result","result":"model is not supported","is_error":true,"session_id":"s"}}'
  echo 'mcp login required' >&2
  exit 1
fi
printf '%s' "$answer" | python3 -c 'import json,sys; print(json.dumps({{"type":"result","result":sys.stdin.read(),"is_error":False,"session_id":"s"}}))'
"#,
            d = dir.display()
        ),
    )?;
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o700))?;
    Ok(Profile {
        client: Client::Claude,
        executable: bin.display().to_string(),
        model: String::new(),
        check_command: String::new(),
    })
}

fn setup(home: &Path, profile: &Profile) -> Result<(String, String)> {
    let mut lib = Library::open(home)?;
    lib.change(home, |lib| {
        lib.create("auto", vec![])?;
        let p = lib.project_mut()?;
        p.add_work("导出", Kind::Feature, Template::Software)?;
        let pid = p.id.clone();
        let w = p.selected_mut()?;
        for i in 0..7 {
            w.profiles.insert(i, profile.clone());
        }
        w.notes[0] = "支持导出对账单".into();
        Ok((pid, w.id.clone()))
    })
}

fn run(home: &Path, pid: &str, wid: &str, until: usize) -> Result<autopilot::State> {
    let out = Command::new(env!("CARGO_BIN_EXE_aep"))
        .args(["workflow-auto", "--home"])
        .arg(home)
        .args([
            "--project",
            pid,
            "--work",
            wid,
            "--until",
            &until.to_string(),
        ])
        .output()?;
    ensure!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(autopilot::read(&autopilot::work_dir(home, pid, wid)).unwrap())
}

fn stage(home: &Path, wid: &str) -> Result<aep::project::Work> {
    let lib = Library::open(home)?;
    Ok(lib
        .project()
        .unwrap()
        .work
        .iter()
        .find(|w| w.id == wid)
        .unwrap()
        .clone())
}

#[test]
fn runs_every_stage_to_delivery_and_can_stop_early() -> Result<()> {
    let t = tempfile::tempdir()?;
    let home = t.path().join("catalog");
    let (pid, wid) = setup(&home, &fake(t.path())?)?;
    let state = run(&home, &pid, &wid, 3)?;
    assert_eq!(state.status, "done", "{state:?}");
    assert_eq!(
        stage(&home, &wid)?.stage,
        3,
        "stops before the first stage it may not run"
    );
    let state = run(&home, &pid, &wid, 7)?;
    assert_eq!(state.status, "done", "{state:?}");
    assert_eq!(stage(&home, &wid)?.stage, 7);
    Ok(())
}

#[test]
fn problems_go_back_to_the_stage_that_owns_them() -> Result<()> {
    let t = tempfile::tempdir()?;
    let home = t.path().join("catalog");
    let (pid, wid) = setup(&home, &fake(t.path())?)?;
    // The design is unsure about scope; the first test run finds a bug.
    fs::write(
        t.path().join("技术方案.1"),
        "导出范围待定\nAEP-BACK: 产品需求：空账户是否导出表头？",
    )?;
    fs::write(t.path().join("测试验证.1"), "导出缺少表头\nAEP: FAIL")?;
    let state = run(&home, &pid, &wid, 7)?;
    assert_eq!(state.status, "done", "{state:?}");
    let work = stage(&home, &wid)?;
    assert_eq!(work.stage, 7);
    assert_eq!(work.reworks_between(2, 0), 1);
    assert_eq!(work.reworks_between(4, 3), 1);
    // The product stage was asked the exact question in its own session.
    let asked = fs::read_to_string(t.path().join("continue.1.prompt"))?;
    assert!(asked.contains("空账户是否导出表头"), "{asked}");
    // The build was handed the test report itself, not only a file path.
    let prompts: String = fs::read_dir(t.path())?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".prompt"))
        .map(|e| fs::read_to_string(e.path()).unwrap_or_default())
        .collect();
    assert!(
        prompts.contains("的完整报告") && prompts.contains("导出缺少表头"),
        "{prompts}"
    );
    Ok(())
}

#[test]
fn a_blocked_gate_stops_for_a_person() -> Result<()> {
    let t = tempfile::tempdir()?;
    let home = t.path().join("catalog");
    let (pid, wid) = setup(&home, &fake(t.path())?)?;
    fs::write(
        t.path().join("测试验证.1"),
        "沙箱禁止监听端口\nAEP: BLOCKED",
    )?;
    let state = run(&home, &pid, &wid, 7)?;
    assert_eq!(state.status, "stopped", "{state:?}");
    assert!(state.message.contains("BLOCKED"), "{}", state.message);
    let work = stage(&home, &wid)?;
    assert_eq!(work.stage, 4);
    assert!(
        work.reworks.is_empty(),
        "never sends an environment block back"
    );
    Ok(())
}

#[test]
fn planning_owns_specs_skips_optional_stages_and_builds_task_by_task() -> Result<()> {
    let t = tempfile::tempdir()?;
    let home = t.path().join("catalog");
    let repo = t.path().join("repo");
    fs::create_dir_all(&repo)?;
    let git = |args: &[&str]| -> Result<()> {
        let out = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .output()?;
        ensure!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(())
    };
    git(&["init", "-q", "-b", "main"])?;
    fs::write(repo.join("README.md"), "demo\n")?;
    git(&["add", "."])?;
    git(&["commit", "-q", "-m", "init"])?;
    let d = t.path().display();
    let bin = t.path().join("claude");
    fs::write(
        &bin,
        format!(
            r#"#!/bin/sh
input=$(cat)
stage=$(printf '%s' "$input" | sed -n 's/^阶段：\([^（]*\)（.*/\1/p' | head -1)
printf '%s' "$input" | grep -q '子任务路径在仓库中找不到' && stage=修正路径
answer="$stage 完成"
case "$stage" in
  产品需求) mkdir -p specs/demo; echo prd > specs/demo/spec.md
    answer="PRD 完成
AEP-SKIP: 交互设计：无界面改动" ;;
  技术方案) printf '%s' '{{"title":"demo","subtasks":[{{"id":"T1","title":"导出","paths":["lib/exprot/csv.rs"]}},{{"id":"T2","title":"表头"}}]}}' > specs/demo/task.json ;;
  修正路径) echo fixed >> "{d}/path-fix"
    printf '%s' '{{"title":"demo","subtasks":[{{"id":"T1","title":"导出","paths":["src","README.md"]}},{{"id":"T2","title":"表头"}}]}}' > specs/demo/task.json ;;
  开发实现) id=$(printf '%s' "$input" | sed -n 's/.*本轮只实现子任务 \(T[0-9]*\).*/\1/p' | head -1)
    echo "$id" >> "{d}/built"; mkdir -p src; echo "$id" > "src/$id.txt" ;;
  测试验证) echo report > specs/demo/testing.md ;;
esac
printf '%s
AEP: PASS' "$answer" | python3 -c 'import json,sys; print(json.dumps({{"type":"result","result":sys.stdin.read(),"is_error":False,"session_id":"s"}}))'
"#
        ),
    )?;
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o700))?;
    let profile = Profile {
        client: Client::Claude,
        executable: bin.display().to_string(),
        model: String::new(),
        check_command: String::new(),
    };
    let root = fs::canonicalize(&repo)?;
    let mut lib = Library::open(&home)?;
    let (pid, wid) = lib.change(&home, |lib| {
        lib.create("docs", vec![root.clone()])?;
        let p = lib.project_mut()?;
        p.add_work("导出", Kind::Feature, Template::Software)?;
        let pid = p.id.clone();
        let w = p.selected_mut()?;
        w.include(&root);
        for i in 0..7 {
            w.profiles.insert(i, profile.clone());
        }
        Ok((pid, w.id.clone()))
    })?;
    let state = run(&home, &pid, &wid, 7)?;
    assert_eq!(state.status, "done", "{state:?}");
    let work = stage(&home, &wid)?;
    assert_eq!(work.stage, 7);
    // 交互设计 was suggested away by the PRD and never ran an agent.
    assert!(work.skipped.contains_key(&1), "{:?}", work.skipped);
    assert!(work.notes[1].starts_with("已跳过"), "{}", work.notes[1]);
    assert!(!work.attempts.iter().any(|a| a.stage == 1));
    // The plan first named a path that does not exist; its own session was asked to fix it.
    assert_eq!(fs::read_to_string(t.path().join("path-fix"))?, "fixed\n");
    let plan = work
        .attempts
        .iter()
        .find(|a| a.stage == 2)
        .expect("技术方案未运行");
    let first = aep::workflow::evidence(&aep::workflow::attempt_dir(&home, &pid, &wid, &plan.id))?;
    assert_eq!(first["status"], "failed", "{first}");
    assert_eq!(
        first["invalid_paths"],
        serde_json::json!(["T1 lib/exprot/csv.rs"])
    );
    // The build ran once per planned subtask, in order.
    assert_eq!(fs::read_to_string(t.path().join("built"))?, "T1\nT2\n");
    // The test stage's report left specs/ and went into its own run archive.
    let ws = aep::workflow::evidence(&aep::workflow::attempt_dir(
        &home,
        &pid,
        &wid,
        &work.current_attempt[&4],
    ))?;
    let workspace = Path::new(ws["workspace"].as_str().unwrap());
    assert!(!workspace.join("specs/demo/testing.md").exists());
    assert!(workspace.join("specs/demo/task.json").exists());
    assert!(
        aep::workflow::attempt_dir(&home, &pid, &wid, &work.current_attempt[&4])
            .join("spec-drafts/specs/demo/testing.md")
            .is_file()
    );
    assert_eq!(
        ws["spec_drafts"],
        serde_json::json!(["specs/demo/testing.md"])
    );

    // Planning and build output was committed to the item branch, one commit per run.
    let log = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["log", "--format=%s%n%b", "main..HEAD"])
        .output()?;
    let log = String::from_utf8(log.stdout)?;
    assert_eq!(log.matches("AEP-Run:").count(), 4, "{log}");
    assert!(log.contains("导出 · 开发实现"), "{log}");
    // Gates ran on that exact, clean commit, and delivery merges it.
    assert_eq!(ws["clean"], true);
    let head = ws["head_sha"].as_str().unwrap();
    let delivery = work.deliveries.first().expect("交付未封存");
    let candidate = aep::project::merge::candidate(&home, delivery)?;
    assert_eq!(candidate.sha, head);
    assert_eq!(candidate.repository, root);
    assert_eq!(candidate.default_target.as_deref(), Some("main"));
    Ok(())
}

#[test]
fn a_crashing_cli_is_retried_once_and_never_sent_back() -> Result<()> {
    let t = tempfile::tempdir()?;
    let home = t.path().join("catalog");
    let (pid, wid) = setup(&home, &fake(t.path())?)?;
    // A transient crash is retried; the gate still reaches its own verdict.
    fs::write(t.path().join("测试验证.1.crash"), "")?;
    let state = run(&home, &pid, &wid, 7)?;
    assert_eq!(state.status, "done", "{state:?}");
    let work = stage(&home, &wid)?;
    assert!(work.reworks.is_empty(), "a crash is not a failed test");

    // Crashing twice stops with the CLI's own reason, not stderr noise.
    let t = tempfile::tempdir()?;
    let home = t.path().join("catalog");
    let (pid, wid) = setup(&home, &fake(t.path())?)?;
    for n in 1..=2 {
        fs::write(t.path().join(format!("产品需求.{n}.crash")), "")?;
    }
    let state = run(&home, &pid, &wid, 7)?;
    assert_eq!(state.status, "stopped", "{state:?}");
    assert!(
        state.message.contains("model is not supported"),
        "{state:?}"
    );
    assert_eq!(stage(&home, &wid)?.stage, 0);
    Ok(())
}

#[test]
fn archiving_a_delivered_item_writes_project_memory() -> Result<()> {
    let t = tempfile::tempdir()?;
    let home = t.path().join("catalog");
    let profile = fake(t.path())?;
    let (pid, wid) = setup(&home, &profile)?;
    let mut lib = Library::open(&home)?;
    let early = lib.change(&home, |lib| {
        lib.project_mut()?.archive_work(&home, &wid, "x")
    });
    assert!(early.is_err(), "only delivered items can be archived");
    assert_eq!(run(&home, &pid, &wid, 7)?.status, "done");

    let mut lib = Library::open(&home)?;
    let draft = stage(&home, &wid)?.memory_draft(&home, &pid)?;
    assert!(draft.starts_with("「导出」已交付"), "{draft}");
    assert!(draft.contains("- 技术方案："), "{draft}");
    assert!(draft.chars().count() <= aep::memory::TEXT_LIMIT);
    let note = lib.change(&home, |lib| {
        lib.project_mut()?
            .archive_work(&home, &wid, "导出统一用 UTF-8 BOM")
    })?;
    let work = stage(&home, &wid)?;
    assert_eq!(work.shelved.as_ref().and_then(|s| s.memory), note);
    let mut lib = Library::open(&home)?;

    // A later item's stages read the archived decision.
    let next = lib.change(&home, |lib| {
        let p = lib.project_mut()?;
        p.add_work("对账", Kind::Feature, Template::Software)?;
        let w = p.selected_mut()?;
        for i in 0..7 {
            w.profiles.insert(i, profile.clone());
        }
        Ok(w.id.clone())
    })?;
    assert_eq!(run(&home, &pid, &next, 1)?.status, "done");
    let prompt = fs::read_to_string(t.path().join("产品需求.2.prompt"))?;
    assert!(prompt.contains("导出统一用 UTF-8 BOM"), "{prompt}");

    let mut lib = Library::open(&home)?;
    lib.change(&home, |lib| lib.project_mut()?.unarchive_work(&wid))?;
    assert!(stage(&home, &wid)?.shelved.is_none());
    Ok(())
}

#[test]
fn a_delivery_sent_back_rechecks_and_still_seals() -> Result<()> {
    let t = tempfile::tempdir()?;
    let home = t.path().join("catalog");
    let (pid, wid) = setup(&home, &fake(t.path())?)?;
    // Delivery finds the work unfinished; build fixes it, then 测试/验收/交付 re-check in
    // their own sessions and the delivery must still prove it read the new acceptance.
    fs::write(
        t.path().join("交付归档.1"),
        "工作区未提交\nAEP-BACK: 开发实现：请提交实现",
    )?;
    let state = run(&home, &pid, &wid, 7)?;
    assert_eq!(state.status, "done", "{state:?}");
    let work = stage(&home, &wid)?;
    assert_eq!(work.stage, 7);
    assert_eq!(work.reworks_between(6, 3), 1, "{:?}", work.reworks);
    let delivery = work.deliveries.first().expect("交付未封存");
    let accepted = work.incoming(6).expect("缺少验收交接");
    assert_eq!(delivery.accepted_run, accepted.run);
    let ev: serde_json::Value = serde_json::from_slice(&fs::read(
        aep::workflow::attempt_dir(&home, &pid, &wid, &delivery.run).join("evidence.json"),
    )?)?;
    assert!(
        ev["follow_up"].is_object(),
        "delivery re-checked in its session: {ev}"
    );
    assert_eq!(ev["source_evidence"]["run"], accepted.run.as_str(), "{ev}");
    Ok(())
}
