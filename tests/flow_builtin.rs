use aep::{
    flow::{Back, Flow, builtin, builtin_by_id, template_id, validate},
    workflow::{Profile, Sandbox, Template},
};
use anyhow::Result;
use serde::Deserialize;
use serde_json::{Map, Value, json};

#[test]
fn builtin_equivalence() -> Result<()> {
    let expected: Value = serde_json::from_str(include_str!("fixtures/builtin_flows.json"))?;
    let mut actual = Map::new();
    for template in [Template::Legacy, Template::Software, Template::Game] {
        let flow = builtin(template);
        assert_eq!(flow.id, template_id(template));
        assert!(std::ptr::eq(builtin_by_id(&flow.id).unwrap(), flow));

        let stages: Vec<_> = flow
            .stages
            .iter()
            .enumerate()
            .map(|(i, stage)| {
                assert!(stage.back_to.is_empty());
                assert_eq!(stage.skippable, flow.skippable(i));
                json!({
                    "name": stage.name,
                    "brief": stage.brief,
                    "role": stage.role,
                    "skippable": flow.skippable(i),
                    "default_cli": Profile::for_stage(stage).client,
                    "default_sandbox": Sandbox::for_role(stage.role),
                })
            })
            .collect();
        actual.insert(flow.id.clone(), Value::Array(stages));
    }
    assert_eq!(Value::Object(actual), expected);
    assert!(builtin_by_id("missing").is_none());
    Ok(())
}

#[derive(Deserialize)]
struct ValidationCase {
    name: String,
    flow: Flow,
    taken_labels: Vec<String>,
    errors: Vec<Value>,
    warnings: Vec<Value>,
}

#[test]
fn validation_cases() -> Result<()> {
    let cases: Vec<ValidationCase> =
        serde_json::from_str(include_str!("fixtures/flow_cases.json"))?;
    for case in cases {
        let taken: Vec<_> = case.taken_labels.iter().map(String::as_str).collect();
        let actual = validate(&case.flow, &taken);
        assert_eq!(
            serde_json::to_value(actual.errors)?,
            json!(case.errors),
            "{}",
            case.name
        );
        assert_eq!(
            serde_json::to_value(actual.warnings)?,
            json!(case.warnings),
            "{}",
            case.name
        );
    }
    Ok(())
}

#[test]
fn normalized_trimmed_names_remain_addressable() {
    let mut flow = builtin(Template::Software).clone();
    flow.label = "  后端开发  ".into();
    flow.stages[1].name = "  交互设计　".into();
    flow.stages[1].brief = " 保留计划说明 ".into();
    flow.stages[3].back_to = vec!["　交互设计  ".into()];

    let normalized = flow.normalized();
    assert_eq!(normalized.label, "后端开发");
    assert_eq!(normalized.stages[1].name, "交互设计");
    assert_eq!(normalized.stages[1].brief, " 保留计划说明 ");
    assert_eq!(normalized.stages[3].back_to, ["交互设计"]);
    assert_eq!(normalized.back_targets(3), [1]);
    assert!(validate(&normalized, &[]).errors.is_empty());
    assert_eq!(normalized.normalized(), normalized);
    assert_eq!(flow.stages[1].name, "  交互设计　");
}

#[test]
fn skips_only_later_optional_planning_stages() {
    let flow = builtin(Template::Software);
    let result = concat!(
        "  AEP-SKIP: <交互设计>：更早的阶段\n",
        "  AEP-SKIP: 「技术方案」: 无需单独方案  \n",
        "AEP-SKIP: 测试验证：测试不可跳过\n",
        "AEP-SKIP: 旧阶段：未知阶段\n",
    );
    assert_eq!(flow.skips(1, result), [(2, "无需单独方案".into())]);
    assert_eq!(flow.skips(1, "AEP-SKIP: 技术方案"), [(2, String::new())]);
    assert!(flow.skips(2, result).is_empty());
}

#[test]
fn route_back_uses_last_directive_and_allowed_earlier_targets() {
    let mut flow = builtin(Template::Software).clone();
    assert_eq!(flow.back_targets(4), [0, 1, 2, 3]);
    assert!(flow.back_targets(0).is_empty());
    let result = "AEP-BACK: <产品需求>: 另一个问题\n  AEP-BACK: 「开发实现」：补充测试  \n";
    assert_eq!(
        flow.route_back(4, result),
        Back::To {
            stage: 3,
            question: "补充测试".into(),
        }
    );
    flow.stages[4].back_to = vec!["技术方案".into(), "产品需求".into()];
    assert_eq!(flow.back_targets(4), [0, 2]);
    assert_eq!(
        flow.route_back(4, result),
        Back::Rejected {
            name: "开发实现".into(),
            question: "补充测试".into(),
            allowed: vec!["产品需求".into(), "技术方案".into()],
        }
    );
    assert_eq!(
        flow.route_back(4, "AEP-BACK: <产品需求>: 明确范围"),
        Back::To {
            stage: 0,
            question: "明确范围".into(),
        }
    );
    assert_eq!(
        flow.route_back(4, "AEP-BACK: 技术方案：修正\nAEP-BACK: 旧阶段：未知"),
        Back::None
    );
    assert_eq!(flow.route_back(4, "AEP-BACK: 测试验证"), Back::None);
    assert_eq!(flow.route_back(4, "AEP-BACK: 交付归档"), Back::None);
    assert_eq!(flow.route_back(4, "普通阶段结果"), Back::None);
}
