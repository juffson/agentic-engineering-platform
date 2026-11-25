use aep::{
    handoff,
    project::{Library, Work},
    workflow::{Profile, Request, Sandbox, Template},
};
use anyhow::{Context, Result};
use serde_json::{Map, Value, json};
use std::{fs, path::Path};

const TEMPLATES: [(Template, &str); 3] = [
    (Template::Legacy, "Legacy"),
    (Template::Software, "Software"),
    (Template::Game, "Game"),
];

fn check_fixture(name: &str, actual: &str) -> Result<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    if std::env::var("AEP_BLESS").as_deref() == Ok("1") {
        fs::create_dir_all(path.parent().context("fixture must have a parent")?)?;
        fs::write(&path, actual).with_context(|| format!("write {}", path.display()))?;
    }
    let expected = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    assert_eq!(actual, expected, "fixture {}", path.display());
    Ok(expected)
}

fn check_json_fixture(name: &str, actual: &Value) -> Result<String> {
    check_fixture(
        name,
        &format!("{}\n", serde_json::to_string_pretty(actual)?),
    )
}

fn legacy_work(id: &str, template: &str, stage: usize, notes: &[&str]) -> Value {
    json!({
        "id": id,
        "name": id,
        "kind": "Feature",
        "template": template,
        "stage": stage,
        "notes": notes,
        "repositories": [],
    })
}

fn legacy_request_data() -> Value {
    json!({
        "project": "流程基线",
        "work": "兼容旧事项",
        "template": "Software",
        "stage": 2,
        "profile": {
            "client": "Claude",
            "executable": "",
            "model": "",
        },
        "brief": "核对内置流程与阶段交接。",
        "approved": [],
    })
}

#[test]
fn builtin_equivalence() -> Result<()> {
    let mut flows = Map::new();
    for (template, id) in TEMPLATES {
        let stages = template.stages();
        let notes = vec![""; stages.len()];
        let work: Work = serde_json::from_value(legacy_work("baseline", id, 0, &notes))?;
        let stages: Vec<_> = stages
            .iter()
            .enumerate()
            .map(|(i, stage)| {
                json!({
                    "name": stage.name,
                    "brief": stage.brief,
                    "role": format!("{:?}", stage.role),
                    "skippable": work.skippable(i),
                    "default_cli": Profile::for_stage(stage).client,
                    "default_sandbox": Sandbox::for_role(stage.role),
                })
            })
            .collect();
        flows.insert(id.into(), Value::Array(stages));
    }
    check_json_fixture("builtin_flows.json", &Value::Object(flows))?;
    Ok(())
}

#[test]
fn handoff_snapshot() -> Result<()> {
    for (template, id) in TEMPLATES {
        for i in 0..template.stages().len() {
            let mut data = legacy_request_data();
            data["template"] = json!(id);
            data["stage"] = json!(i);
            let req: Request = serde_json::from_value(data)?;
            let text = handoff::header(&req)? + &handoff::rules(&req)?;
            check_fixture(&format!("handoff/{id}-{i}.txt"), &text)?;
        }
    }
    Ok(())
}

#[test]
fn legacy_library() -> Result<()> {
    let mut shelved = legacy_work("shelved", "Game", 7, &["通过"; 7]);
    shelved["shelved"] = json!({"at": "2026-01-01T00:00:00Z"});
    let data = json!({
        "revision": 1,
        "active": "baseline",
        "projects": [{
            "id": "baseline",
            "name": "流程基线",
            "repositories": [],
            "selected": "in-progress",
            "work": [
                legacy_work("in-progress", "Legacy", 2, &["需求", "方案", "", "", ""]),
                legacy_work("delivered", "Software", 7, &["通过"; 7]),
                shelved,
            ],
        }],
    });
    let fixture = check_json_fixture("legacy_projects.json", &data)?;
    let tmp = tempfile::tempdir()?;
    let home = tmp.path();
    let projects_path = home.join("projects.json");
    fs::write(&projects_path, fixture)?;

    let mut library = Library::open(home)?;
    let project = library.project().context("missing legacy project")?;
    let [in_progress, delivered, shelved] = project.work.as_slice() else {
        anyhow::bail!("expected three legacy work items");
    };
    assert_eq!(in_progress.template, Template::Legacy);
    assert_eq!(in_progress.stage_name(), "开发实现");
    assert!(!in_progress.delivered());
    assert!(in_progress.shelved.is_none());
    assert_eq!(delivered.template, Template::Software);
    assert_eq!(delivered.stage_name(), "已交付");
    assert!(delivered.delivered());
    assert!(delivered.shelved.is_none());
    assert_eq!(shelved.template, Template::Game);
    assert_eq!(shelved.stage_name(), "已交付");
    assert!(shelved.delivered());
    assert!(shelved.shelved.is_some());

    library.change(home, |library| {
        library
            .project_mut()?
            .finish_stage(home, "in-progress", 2, "实现完成".into())
    })?;

    let saved: Value = serde_json::from_slice(&fs::read(projects_path)?)?;
    let work = saved["projects"][0]["work"]
        .as_array()
        .context("missing saved work items")?;
    assert_eq!(work.len(), 3);
    for item in work {
        assert!(item.get("flow").is_none(), "{}", item["id"]);
        assert!(item.get("flow_error").is_none(), "{}", item["id"]);
    }

    let library = Library::open(home)?;
    let current = library
        .project()
        .and_then(|project| project.selected())
        .context("missing saved selection")?;
    assert_eq!(current.stage, 3);
    assert_eq!(current.stage_name(), "测试审查");
    assert!(!current.delivered());
    assert_eq!(current.notes, ["需求", "方案", "实现完成", "", ""]);
    Ok(())
}

#[test]
fn legacy_request() -> Result<()> {
    let fixture = check_json_fixture("legacy_request.json", &legacy_request_data())?;
    let req: Request = serde_json::from_str(&fixture)?;
    assert_eq!(req.template, Template::Software);
    assert_eq!(req.stage, 2);
    let header = handoff::header(&req)?;
    assert!(
        header.contains("\n阶段：技术方案（软件开发）\n"),
        "{header}"
    );
    Ok(())
}
