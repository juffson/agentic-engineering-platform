use aep::{
    flow::{Flow, FlowStage, builtin},
    handoff::{self, Assignment},
    project::{self, Kind, Library, Project, ProjectFlow, Work, save_json},
    workflow::{Client, Profile, Request, Role, Sandbox, Template},
    workspace::{git, read},
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

#[path = "support/custom_workflow.rs"]
mod automatic;

fn project_flow<'a>(project: &'a Project, id: &str) -> &'a ProjectFlow {
    project
        .flows
        .iter()
        .find(|entry| entry.flow.id == id)
        .unwrap()
}

fn custom_legacy(label: &str) -> Flow {
    let mut flow = builtin(Template::Legacy).clone();
    flow.id.clear();
    flow.label = label.into();
    flow
}

fn security_review() -> FlowStage {
    FlowStage {
        name: "安全审查".into(),
        brief: "核对权限边界。".into(),
        role: Role::Test,
        skippable: false,
        back_to: vec!["开发实现".into()],
    }
}

fn backend_flow() -> Flow {
    let mut flow = builtin(Template::Software).clone();
    flow.id.clear();
    flow.label = "后端开发".into();
    flow.stages.remove(1);
    flow.stages.insert(4, security_review());
    flow
}

fn project_cli(home: &Path, args: &[&str]) -> Result<Output> {
    Ok(Command::new(env!("CARGO_BIN_EXE_aep"))
        .current_dir(home)
        .env_remove("AEP_PROJECT")
        .env_remove("AEP_WORK")
        .env_remove("AEP_PROJECT_HOME")
        .args(["project", "--home"])
        .arg(home)
        .args(args)
        .output()?)
}

fn cli_json(home: &Path, args: &[&str]) -> Result<Value> {
    let out = project_cli(home, args)?;
    ensure!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(serde_json::from_slice(&out.stdout)?)
}

fn setup_automatic(
    root: &Path,
    flow: Flow,
    repo: Option<PathBuf>,
) -> Result<(PathBuf, String, String)> {
    let home = root.join("catalog");
    let profile = automatic::fake(root)?;
    let mut lib = Library::open(&home)?;
    let (pid, wid) = lib.change(&home, |lib| {
        lib.create("自动运行", repo.clone().into_iter().collect())?;
        let p = lib.project_mut()?;
        let pid = p.id.clone();
        let (id, _) = p.save_flow(flow)?;
        p.add_work_flow("接口改动", Kind::Feature, &id)?;
        let work = p.selected_mut()?;
        if let Some(repo) = &repo {
            work.include(repo);
        }
        for i in 0..work.stages().len() {
            work.profiles.insert(i, profile.clone());
        }
        Ok((pid, work.id.clone()))
    })?;
    Ok((home, pid, wid))
}

fn selected_work(home: &Path) -> Result<Work> {
    Ok(Library::open(home)?
        .project()
        .unwrap()
        .selected()
        .unwrap()
        .clone())
}

#[test]
fn runs_to_delivery() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let repo = dir.path().join("repo");
    fs::create_dir(&repo)?;
    let repo = repo.canonicalize()?;
    git(&repo, &["init", "-q", "-b", "main"])?;
    git(
        &repo,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "--allow-empty",
            "-qm",
            "initial",
        ],
    )?;
    let flow = backend_flow();
    let (home, pid, wid) = setup_automatic(dir.path(), flow.clone(), Some(repo.clone()))?;
    let state = automatic::run(&home, &pid, &wid, 7)?;
    assert_eq!(state.status, "done", "{state:?}");
    let work = selected_work(&home)?;
    assert!(work.delivered());
    assert_eq!(work.stage, 7);
    assert_eq!(
        work.attempts.iter().map(|a| a.stage).collect::<Vec<_>>(),
        (0..7).collect::<Vec<_>>()
    );

    let review_run = aep::workflow::attempt_dir(&home, &pid, &wid, &work.current_attempt[&4]);
    let req: Request = serde_json::from_value(read(&review_run.join("request.json"))?)?;
    assert_eq!(req.flow().stages[req.stage], security_review());
    assert_eq!(req.sandbox, Sandbox::Network);
    let args = fs::read_to_string(dir.path().join("安全审查.1.args"))?;
    assert!(args.contains("--tools\nRead,Grep,Glob,Bash\n"));
    assert!(args.contains("--allowedTools\nBash\n"));
    let review = aep::workflow::evidence(&review_run)?;
    assert_eq!(review["clean"], true);
    let test = aep::workflow::evidence(&aep::workflow::attempt_dir(
        &home,
        &pid,
        &wid,
        &work.current_attempt[&3],
    ))?;
    assert_eq!(review["head_sha"], test["head_sha"]);
    assert!(
        work.incoming(5)
            .unwrap()
            .acceptance
            .starts_with("AEP: PASS")
    );

    let mut lib = Library::open(&home)?;
    let delivery = &work.deliveries[0];
    let names: Vec<_> = flow.stages.iter().map(|s| &s.name).collect();
    let timeline = delivery.snapshot(&home)?;
    assert_eq!(timeline.stages.iter().collect::<Vec<_>>(), names);
    assert_eq!(timeline.rounds[4].agents[0].role, "安全审查");
    let preview = project::merge::preview(&home, delivery, "main")?;
    assert!(preview.branch.writable());
    assert_eq!(preview.candidate.transfer_run, work.current_attempt[&5]);
    assert_eq!(preview.candidate.sha, review["head_sha"].as_str().unwrap());
    assert!(preview.diff.contains("+backend ready"));
    let receipt = project::merge::merge(
        &home,
        &mut lib,
        &pid,
        &wid,
        &delivery.run,
        "main",
        &preview.diff_sha256,
    )?;
    assert_eq!(receipt.status, project::merge::MergeStatus::Merged);
    assert_eq!(
        fs::read_to_string(repo.join("backend.txt"))?,
        "backend ready\n"
    );
    Ok(())
}

#[test]
fn skip_rules() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (home, pid, wid) = setup_automatic(dir.path(), backend_flow(), None)?;
    fs::write(
        dir.path().join("产品需求.1"),
        "AEP-SKIP: 技术方案：已有方案\nAEP-SKIP: 安全审查：声称无需审查",
    )?;
    let state = automatic::run(&home, &pid, &wid, 7)?;
    assert_eq!(state.status, "done", "{state:?}");
    let work = selected_work(&home)?;
    assert_eq!(work.skipped.len(), 1);
    assert_eq!(work.skipped[&1], "产品需求：已有方案");
    assert!(!work.attempts.iter().any(|a| a.stage == 1));
    assert!(work.attempts.iter().any(|a| a.stage == 4));
    Ok(())
}

#[test]
fn autopilot_gate_failure() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut flow = backend_flow();
    flow.stages[4].back_to = vec!["产品需求".into()];
    let (home, pid, wid) = setup_automatic(dir.path(), flow.clone(), None)?;
    fs::write(dir.path().join("安全审查.1"), "权限核验失败\nAEP: FAIL")?;
    let state = automatic::run(&home, &pid, &wid, 7)?;
    assert_eq!(state.status, "done", "{state:?}");
    let work = selected_work(&home)?;
    assert_eq!(work.reworks_between(4, 2), 1);
    let rework = &work.reworks[0];
    let failed = aep::workflow::attempt_dir(&home, &pid, &wid, rework.run.as_deref().unwrap());
    assert_eq!(aep::workflow::evidence(&failed)?["status"], "failed");
    let prompt = fs::read_to_string(dir.path().join("开发实现.2.prompt"))?;
    assert!(prompt.contains("权限核验失败"));
    let prompt = fs::read_to_string(dir.path().join("安全审查.2.prompt"))?;
    assert!(prompt.contains("最近一次退回：「安全审查」→「开发实现」"));

    let dir = tempfile::tempdir()?;
    flow.stages.remove(2);
    let (home, pid, wid) = setup_automatic(dir.path(), flow, None)?;
    fs::write(dir.path().join("安全审查.1"), "权限核验失败\nAEP: FAIL")?;
    let state = automatic::run(&home, &pid, &wid, 6)?;
    assert_eq!(state.status, "stopped", "{state:?}");
    assert!(state.message.contains("「安全审查」未通过"));
    let work = selected_work(&home)?;
    assert_eq!(work.stage_name(), "安全审查");
    assert!(work.reworks.is_empty());
    Ok(())
}

fn reserve_request(
    home: &Path,
    flow: Flow,
    stage: usize,
    answer: &str,
) -> Result<(Library, PathBuf)> {
    let executable = home.join("stage-agent");
    fs::write(
        &executable,
        format!("#!/bin/sh\ncat <<'AEP_TEST_RESULT'\n{answer}\nAEP_TEST_RESULT\n"),
    )?;
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))?;
    let mut library = Library::open(home)?;
    let dir = library.change(home, |library| {
        library.create("流程执行", vec![])?;
        let project = library.project_mut()?;
        let (id, _) = project.save_flow(flow)?;
        project.add_work_flow("执行自定义阶段", Kind::Feature, &id)?;
        let work = project.selected_mut()?;
        work.stage = stage;
        work.notes[1] = "已确认接口".into();
        let work_id = work.id.clone();
        project.reserve(
            home,
            &work_id,
            stage,
            Assignment {
                profile: Profile {
                    client: Client::Custom,
                    executable: executable.display().to_string(),
                    model: String::new(),
                    check_command: String::new(),
                },
                brief: "核对结果".into(),
                repository: None,
                external: None,
            },
        )
    })?;
    Ok((library, dir))
}

fn execute_request(dir: &Path, success: bool) -> Result<Value> {
    let output = Command::new(env!("CARGO_BIN_EXE_aep"))
        .args(["workflow-stage", "--request"])
        .arg(dir.join("request.json"))
        .output()?;
    let evidence = aep::workflow::evidence(dir)?;
    assert_eq!(
        output.status.success(),
        success,
        "{evidence}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(evidence)
}

#[test]
fn request_self_contained() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let home = dir.path();
    let mut flow = builtin(Template::Software).clone();
    flow.id.clear();
    flow.label = "后端开发".into();
    flow.stages[1].name = "接口设计".into();
    let stage = flow.stages.len() - 1;
    flow.stages.insert(stage, security_review());
    let (mut library, run) = reserve_request(home, flow.clone(), stage, "审查通过\nAEP: PASS")?;
    let raw: Value = aep::workspace::read(&run.join("request.json"))?;
    assert_eq!(raw["template"], "Software");
    assert!(raw["flow"].is_object());
    let request: Request = serde_json::from_value(raw)?;
    flow.id.clone_from(&request.flow().id);
    assert_eq!(request.flow(), &flow);
    assert_eq!(
        request.approved[1],
        ("接口设计".into(), "已确认接口".into())
    );
    assert_eq!(request.sandbox, Sandbox::Network);

    let work_id = library.change(home, |library| {
        let project = library.project_mut()?;
        assert_eq!(project.delete_flow(&flow.id)?, 1);
        Ok(project.selected().unwrap().id.clone())
    })?;
    let evidence = execute_request(&run, true)?;
    assert_eq!(evidence["stage"], "安全审查");
    assert_eq!(evidence["status"], "succeeded");
    let prompt = fs::read_to_string(run.join("prompt.txt"))?;
    assert!(prompt.contains("阶段：安全审查（后端开发）"));
    assert!(prompt.contains("阶段职责：核对权限边界。"));

    let follow = library.change(home, |library| {
        library
            .project_mut()?
            .reserve_follow_up(home, &work_id, stage, "再核对权限边界")
    })?;
    let continued: Request = serde_json::from_slice(&fs::read(follow.join("request.json"))?)?;
    assert_eq!(continued.flow(), &flow);
    assert_eq!(continued.follow_up.unwrap().message, "再核对权限边界");
    assert_eq!(execute_request(&follow, true)?["stage"], "安全审查");
    Ok(())
}

#[test]
fn back_rules() -> Result<()> {
    let mut flow = builtin(Template::Software).clone();
    flow.id.clear();
    flow.label = "后端开发".into();
    let stage = flow.stages.len() - 1;
    flow.stages.insert(stage, security_review());
    let answer = "AEP-BACK: 开发实现：修正权限边界";

    let dir = tempfile::tempdir()?;
    let (_, run) = reserve_request(
        dir.path(),
        flow.clone(),
        stage,
        &format!("{answer}\nAEP: PASS"),
    )?;
    let evidence = execute_request(&run, false)?;
    assert_eq!(evidence["status"], "failed");
    assert_eq!(
        evidence["route_back"],
        json!({"stage":3,"name":"开发实现","question":"修正权限边界"})
    );
    assert!(evidence["route_back_rejected"].is_null());

    flow.stages[stage].back_to = vec!["产品需求".into()];
    for pass in [true, false] {
        let dir = tempfile::tempdir()?;
        let response = if pass {
            format!("{answer}\nAEP: PASS")
        } else {
            answer.to_owned()
        };
        let (_, run) = reserve_request(dir.path(), flow.clone(), stage, &response)?;
        let request: Request = serde_json::from_slice(&fs::read(run.join("request.json"))?)?;
        let rules = handoff::rules(&request)?;
        assert!(rules.contains("前序阶段（产品需求）"));
        assert!(!rules.contains("前序阶段（开发实现）"));
        let evidence = execute_request(&run, pass)?;
        assert_eq!(
            evidence["status"],
            if pass { "succeeded" } else { "failed" }
        );
        assert!(evidence["route_back"].is_null());
        assert_eq!(
            evidence["route_back_rejected"],
            json!({"name":"开发实现","question":"修正权限边界","allowed":["产品需求"]})
        );
        if !pass {
            assert_eq!(evidence["error"], "验收尚未通过，请查看阶段答复并补齐证据");
        }
    }
    Ok(())
}

#[test]
fn save_and_reload() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let home = dir.path();
    let mut library = Library::open(home)?;
    let id = library.change(home, |library| {
        library.create("流程测试", vec![])?;
        let project = library.project_mut()?;
        let id = project.copy_flow("Software", None)?;
        let copy = project.copy_flow("Software", None)?;
        assert_eq!(project_flow(project, &copy).flow.label, "软件开发 副本 2");
        project.copy_flow("Game", Some("A 游戏流程"))?;
        Ok(id)
    })?;
    let suffix = id.strip_prefix("flow-").unwrap();
    assert_eq!(suffix.len(), 12);
    assert!(suffix.bytes().all(|byte| byte.is_ascii_hexdigit()));

    let mut library = Library::open(home)?;
    let initial = project_flow(library.project().unwrap(), &id);
    assert_eq!(initial.revision, 1);
    assert_eq!(initial.flow.stages, builtin(Template::Software).stages);
    let mut edit = initial.flow.clone();
    edit.label = " 后端开发　".into();
    edit.stages[1].name = " 后端设计 ".into();
    library.change(home, |library| {
        let project = library.project_mut()?;
        let (updated, warnings) = project.save_flow(edit)?;
        assert_eq!(updated, id);
        assert!(warnings.is_empty());
        let copy = project.copy_flow(&updated, None)?;
        assert_eq!(project_flow(project, &copy).flow.label, "后端开发 副本");

        let (_, warnings) = project.save_flow(custom_legacy("无独立验收"))?;
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message.contains("没有验收阶段"));
        assert_eq!(
            project
                .save_flow(custom_legacy(" 后端开发 "))
                .unwrap_err()
                .to_string(),
            "与「后端开发」重名"
        );
        assert_eq!(
            project
                .save_flow(custom_legacy("经典开发流程"))
                .unwrap_err()
                .to_string(),
            "不能与内置流程同名"
        );
        assert_eq!(
            project
                .save_flow(builtin(Template::Software).clone())
                .unwrap_err()
                .to_string(),
            "内置流程不能修改"
        );
        assert_eq!(
            project.delete_flow("Legacy").unwrap_err().to_string(),
            "内置流程不能删除"
        );
        Ok(())
    })?;

    let library = Library::open(home)?;
    let project = library.project().unwrap();
    let saved = project_flow(project, &id);
    assert_eq!(saved.revision, 2);
    assert_eq!(saved.flow.label, "后端开发");
    assert_eq!(saved.flow.stages[1].name, "后端设计");
    chrono::DateTime::parse_from_rfc3339(&saved.updated_at)?;
    let labels: Vec<_> = project
        .flow_choices()
        .iter()
        .map(|flow| flow.label.as_str())
        .collect();
    assert_eq!(
        labels,
        [
            "软件开发",
            "游戏开发",
            "A 游戏流程",
            "后端开发",
            "后端开发 副本",
            "无独立验收",
            "软件开发 副本 2"
        ]
    );
    Ok(())
}

#[test]
fn cli_flow_round_trip() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let home = dir.path();
    cli_json(home, &["create", "CLI 流程"])?;
    let copied = cli_json(home, &["flow-copy", "Software"])?;
    let id = copied["id"].as_str().unwrap();
    let old = cli_json(home, &["flow-show", "软件开发 副本"])?;
    assert_eq!(old["id"], id);
    assert_eq!(old["revision"], 1);
    assert_eq!(old["builtin"], false);
    assert_eq!(
        cli_json(home, &["flow-show", "经典开发流程"])?["id"],
        "Legacy"
    );

    let mut flow = backend_flow();
    flow.id = id.into();
    save_json(home, "flow.json", &flow)?;
    let saved = cli_json(home, &["flow-save", "--file", "flow.json"])?;
    assert_eq!(saved, json!({"id":id,"revision":2,"warnings":[]}));
    let library = Library::open(home)?;
    assert_eq!(&project_flow(library.project().unwrap(), id).flow, &flow);

    let conflict = project_cli(
        home,
        &["add-work", "冲突", "--template", "game", "--flow", id],
    )?;
    assert!(!conflict.status.success());
    assert!(
        !project_cli(home, &["flow-delete", "软件开发"])?
            .status
            .success()
    );
    cli_json(home, &["add-work", "我的事项", "--flow", "后端开发"])?;
    let view = cli_json(home, &["show"])?;
    assert_eq!(view["flow"], json!({"id":id,"label":"后端开发"}));
    assert_eq!(view["stages"][4]["name"], "安全审查");
    let choices = cli_json(home, &["flows"])?;
    let entries = choices["flows"].as_array().unwrap();
    assert_eq!(entries.iter().filter(|f| f["builtin"] == true).count(), 3);
    assert_eq!(entries.iter().find(|f| f["id"] == id).unwrap()["in_use"], 1);
    assert_eq!(choices["broken"], json!([]));
    assert_eq!(
        cli_json(home, &["flow-delete", "后端开发"])?,
        json!({"deleted":id,"in_use":1})
    );
    assert!(
        !project_cli(home, &["add-work", "不可选", "--flow", id])?
            .status
            .success()
    );
    assert_eq!(cli_json(home, &["show"])?["flow"]["id"], id);

    let mut draft = serde_json::to_value(custom_legacy("兼容发布"))?;
    draft.as_object_mut().unwrap().remove("id");
    save_json(home, "flow.json", &draft)?;
    let created = cli_json(home, &["flow-save", "--file", "flow.json"])?;
    assert_eq!(created["revision"], 1);
    assert!(
        created["warnings"][0]
            .as_str()
            .unwrap()
            .contains("没有验收阶段")
    );

    draft["stages"] = json!([]);
    draft["label"] = json!(" ");
    save_json(home, "flow.json", &draft)?;
    let invalid = project_cli(home, &["flow-save", "--file", "flow.json"])?;
    assert!(!invalid.status.success());
    let error = String::from_utf8(invalid.stderr)?;
    assert!(
        error.lines().any(|line| line.contains("至少需要 2 个阶段")),
        "{error}"
    );
    assert!(
        error.lines().any(|line| line.contains("流程名称不能为空")),
        "{error}"
    );
    Ok(())
}

#[test]
fn draft_chooses_project_flow() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    let profile = automatic::fake(root)?;
    let answer = json!({"summary":"检查接口","items":[
        {"name":"接口","template":"flow-backend","brief":"添加接口"},
        {"name":"旧配置","template":"flow-missing","brief":"整理配置"}
    ]});
    save_json(
        root,
        "draft-result.json",
        &json!({"result":answer.to_string(),"is_error":false}),
    )?;

    let mut flow = backend_flow();
    flow.id = "flow-backend".into();
    for custom in [true, false] {
        let job = root.join(if custom { "custom" } else { "legacy" });
        fs::create_dir(&job)?;
        let mut spec = json!({"profile":profile,"request":"新增接口"});
        if custom {
            spec["flows"] = json!([
                aep::work_draft::DraftFlow::from(builtin(Template::Software)),
                aep::work_draft::DraftFlow::from(&flow),
            ]);
        }
        save_json(&job, "job.json", &spec)?;
        aep::work_draft::run(&job)?;
        assert_eq!(read(&job.join("status.json"))?["status"], "succeeded");
        let text = fs::read_to_string(job.join("prompt.txt"))?;
        let draft = read(&job.join("draft.json"))?;
        if custom {
            assert!(text.contains("flow-backend（后端开发）"));
            assert!(text.contains("测试验证 → 安全审查 → 独立验收"));
            assert_eq!(draft["items"][0]["template"], "flow-backend");
            assert!(draft["items"][0].get("fallback_from").is_none());
        } else {
            assert!(text.contains("Software（软件开发）") && text.contains("Game（游戏开发）"));
            assert_eq!(draft["items"][0]["template"], "Software");
            assert_eq!(draft["items"][0]["fallback_from"], "flow-backend");
        }
        assert_eq!(draft["items"][1]["template"], "Software");
        assert_eq!(draft["items"][1]["fallback_from"], "flow-missing");
    }
    Ok(())
}

#[test]
fn snapshot_frozen() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let home = dir.path();
    let mut library = Library::open(home)?;
    let mut original = custom_legacy("冻结流程");
    original.stages[2].name = "后端检查".into();
    original.stages[2].role = Role::Test;
    let (id, work_id, archived_id) = library.change(home, |library| {
        library.create("快照测试", vec![])?;
        let project = library.project_mut()?;
        project.cli_defaults.push(Profile {
            client: Client::Claude,
            executable: "claude-local".into(),
            model: String::new(),
            check_command: String::new(),
        });
        let (id, _) = project.save_flow(original.clone())?;
        project.add_work_flow("进行中", Kind::Feature, &id)?;
        let work = project.selected_mut()?;
        let work_id = work.id.clone();
        work.stage = 2;
        work.current_attempt.insert(2, "in-flight".into());

        project.add_work_flow("已收起", Kind::Feature, &id)?;
        let archived = project.selected_mut()?;
        archived.stage = archived.stages().len() + 1;
        let archived_id = archived.id.clone();
        project.archive_work(home, &archived_id, "")?;
        assert_eq!(project.flow_in_use(&id), 1);
        Ok((id, work_id, archived_id))
    })?;

    let new_work_id = library.change(home, |library| {
        let project = library.project_mut()?;
        let mut edit = project_flow(project, &id).flow.clone();
        edit.stages.remove(1);
        edit.stages[1].name = "新版检查".into();
        project.save_flow(edit)?;
        assert_eq!(project_flow(project, &id).revision, 2);
        let work = project.work_by_id_mut(&work_id)?;
        assert_eq!(work.flow.as_ref().unwrap().source_revision, 1);
        assert_eq!(work.stage_name(), "后端检查");

        project.add_work_flow("新版事项", Kind::Feature, &id)?;
        let latest = project.selected().unwrap();
        assert_eq!(latest.flow.as_ref().unwrap().source_revision, 2);
        assert_eq!(latest.stages()[1].name, "新版检查");
        assert_eq!(latest.notes.len(), 4);
        let latest_id = latest.id.clone();

        assert_eq!(project.delete_flow(&id)?, 2);
        let expected = format!("流程「{id}」已被删除，请重新选择");
        assert_eq!(
            project
                .add_work_flow("无法选择", Kind::Feature, &id)
                .unwrap_err()
                .to_string(),
            expected
        );
        Ok(latest_id)
    })?;

    let mut library = Library::open(home)?;
    let project = library.project_mut()?;
    assert!(project.flow_choices().iter().all(|flow| flow.id != id));
    assert_eq!(project.selected.as_deref(), Some(new_work_id.as_str()));
    assert_eq!(project.flow_in_use(&id), 2);
    let work = project.work_by_id_mut(&work_id)?;
    assert_eq!(work.template, Template::Software);
    assert_eq!(work.flow_id(), id);
    assert_eq!(work.stages(), original.stages);
    assert_eq!(work.notes.len(), 5);
    assert_eq!(work.stage, 2);
    assert_eq!(work.stage_name(), "后端检查");
    assert_eq!(work.current_attempt[&2], "in-flight");
    assert!(work.skippable(1));
    assert!(!work.skippable(2));
    assert_eq!(work.profile(1).executable, "claude-local");
    assert_eq!(work.profile(2).client, Client::Codex);
    assert_eq!(work.profile(5).client, Client::Codex);
    assert_eq!(work.sandbox(2), Sandbox::Network);
    assert!(!work.delivered());
    let archived = project.work_by_id_mut(&archived_id)?;
    assert!(archived.delivered());
    assert!(archived.shelved.is_some());
    assert_eq!(archived.stage_name(), "已交付");
    let pid = project.id.clone();
    let profile = automatic::fake(home)?;
    library.change(home, |library| {
        let work = library.project_mut()?.work_by_id_mut(&work_id)?;
        work.current_attempt.remove(&2);
        for i in 2..work.stages().len() {
            work.profiles.insert(i, profile.clone());
        }
        Ok(())
    })?;
    let state = automatic::run(home, &pid, &work_id, original.stages.len())?;
    assert_eq!(state.status, "done", "{state:?}");
    let work = Library::open(home)?
        .project_mut()?
        .work_by_id_mut(&work_id)?
        .clone();
    assert!(work.delivered());
    assert_eq!(work.stage, original.stages.len());
    assert!(
        work.incoming(3)
            .unwrap()
            .acceptance
            .starts_with("AEP: PASS")
    );
    assert!(home.join("后端检查.1.prompt").is_file());
    Ok(())
}

#[test]
fn broken_snapshot() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let home = dir.path();
    let path = home.join("projects.json");
    let mut raw: Value = serde_json::from_str(include_str!("fixtures/legacy_projects.json"))?;
    let project = &mut raw["projects"][0];
    let mut flow = custom_legacy("可读流程");
    flow.id = "flow-valid".into();
    let valid = serde_json::to_value(ProjectFlow {
        flow,
        revision: 1,
        updated_at: "2026-01-01T00:00:00Z".into(),
    })?;
    let unreadable_definition = json!({"id": "flow-unreadable", "stages": false});
    let mut invalid_definition = valid.clone();
    invalid_definition["id"] = json!("flow-invalid");
    invalid_definition["stages"] = json!([]);
    project["flows"] = json!([valid, unreadable_definition, invalid_definition]);

    let unreadable_snapshot = json!({"id": "flow-unreadable", "stages": false});
    let mut invalid_snapshot = invalid_definition;
    invalid_snapshot.as_object_mut().unwrap().remove("revision");
    invalid_snapshot
        .as_object_mut()
        .unwrap()
        .remove("updated_at");
    invalid_snapshot["source_revision"] = json!(1);
    let corrupt = [
        ("unknown-template", json!("Unknown"), Value::Null),
        (
            "unreadable-snapshot",
            json!("Software"),
            unreadable_snapshot,
        ),
        ("invalid-snapshot", json!("Game"), invalid_snapshot),
    ];
    let work = project["work"].as_array_mut().unwrap();
    for (id, template, snapshot) in &corrupt {
        let mut item = json!({
            "id": id, "name": id, "kind": "Feature", "stage": 10,
            "template": template, "notes": [], "repositories": [],
        });
        if !snapshot.is_null() {
            item["flow"] = snapshot.clone();
        }
        work.push(item);
    }
    work.push(json!({
        "id": "implicit-legacy", "name": "缺省模板", "kind": "Feature",
        "stage": 1, "notes": ["", "", "", "", ""], "repositories": [],
    }));
    let bytes = serde_json::to_vec_pretty(&raw)?;
    fs::write(&path, &bytes)?;

    let mut library = Library::open(home)?;
    assert_eq!(fs::read(&path)?, bytes);
    let project = library.project_mut()?;
    assert_eq!(project.flows.len(), 1);
    assert_eq!(project_flow(project, "flow-valid").flow.label, "可读流程");
    assert_eq!(project.broken_flows.len(), 2);
    assert_eq!(project.broken_flows[0].raw, unreadable_definition);
    assert_eq!(project.broken_flows[1].raw["id"], "flow-invalid");
    assert!(project.broken_flows[1].error.contains("至少需要 2 个阶段"));
    assert_eq!(project.selected().unwrap().stage_name(), "开发实现");
    assert_eq!(
        project.work_by_id_mut("implicit-legacy")?.stage_name(),
        "方案设计"
    );
    for (id, template, snapshot) in &corrupt {
        let item = project.work_by_id_mut(id)?;
        let error = item.flow_error.as_ref().unwrap();
        assert_eq!(&error.template, template);
        assert_eq!(&error.flow, snapshot);
        assert_eq!(item.template, Template::Legacy);
        assert!(item.flow.is_none());
        assert!(
            item.flow()
                .unwrap_err()
                .to_string()
                .starts_with("这个事项的流程数据无法读取：")
        );
        assert!(item.stages().is_empty());
        assert_eq!(item.stage_name(), "流程无法读取");
        assert!(!item.delivered());
        assert!(!item.skippable(1));
        assert_eq!(item.profile(1).client, Client::Codex);
    }
    let view = cli_json(home, &["--work", "unknown-template", "show"])?;
    assert_eq!(view["stages"], json!([]));
    assert!(view["flow_error"].is_string());
    for args in [
        &["advance", "--stage", "1", "--note", "已完成"][..],
        &["reopen", "--stage", "1"][..],
        &["run"][..],
    ] {
        let mut command = vec!["--work", "unknown-template"];
        command.extend_from_slice(args);
        let out = project_cli(home, &command)?;
        assert!(!out.status.success(), "{command:?}");
        assert!(String::from_utf8(out.stderr)?.contains("这个事项的流程数据无法读取"));
    }
    let flows = cli_json(home, &["flows"])?;
    assert_eq!(flows["broken"][0]["id"], "flow-unreadable");

    library.change(home, |library| {
        library
            .project_mut()?
            .add_work_flow("仍可创建", Kind::Feature, "flow-valid")
    })?;
    let mut library = Library::open(home)?;
    let project = library.project_mut()?;
    assert_eq!(project.broken_flows.len(), 2);
    assert_eq!(project.broken_flows[0].raw, unreadable_definition);
    assert_eq!(project.selected().unwrap().flow_id(), "flow-valid");
    for (id, template, snapshot) in &corrupt {
        let error = project.work_by_id_mut(id)?.flow_error.as_ref().unwrap();
        assert_eq!(&error.template, template);
        assert_eq!(&error.flow, snapshot);
    }
    library.change(home, |library| {
        assert_eq!(library.project_mut()?.delete_flow("flow-unreadable")?, 0);
        Ok(())
    })?;
    assert_eq!(
        Library::open(home)?.project().unwrap().broken_flows.len(),
        1
    );
    Ok(())
}
