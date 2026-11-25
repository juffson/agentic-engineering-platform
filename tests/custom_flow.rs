use aep::{
    flow::{Flow, builtin},
    project::{Kind, Library, Project, ProjectFlow},
    workflow::{Client, Profile, Role, Sandbox, Template},
};
use anyhow::Result;
use serde_json::{Value, json};
use std::fs;

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
