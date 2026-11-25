//! Stage definitions, validation, and the directives that move work between stages.
use crate::workflow::{Role, Template};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::OnceLock};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowStage {
    pub name: String,
    pub brief: String,
    pub role: Role,
    #[serde(default)]
    pub skippable: bool,
    /// An empty list allows a return to any earlier stage.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub back_to: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flow {
    pub id: String,
    pub label: String,
    pub stages: Vec<FlowStage>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Back {
    None,
    To {
        stage: usize,
        question: String,
    },
    Rejected {
        name: String,
        question: String,
        allowed: Vec<String>,
    },
}

impl Flow {
    pub fn normalized(&self) -> Self {
        let mut flow = self.clone();
        flow.label = flow.label.trim().to_owned();
        for stage in &mut flow.stages {
            stage.name = stage.name.trim().to_owned();
            for target in &mut stage.back_to {
                *target = target.trim().to_owned();
            }
        }
        flow
    }

    pub fn skippable(&self, i: usize) -> bool {
        i > 0 && self.stages.get(i).is_some_and(|stage| stage.skippable)
    }

    /// Collect AEP-SKIP directives naming optional stages after the current one.
    pub fn skips(&self, from: usize, result: &str) -> Vec<(usize, String)> {
        result
            .lines()
            .filter_map(|line| line.trim().strip_prefix("AEP-SKIP:"))
            .filter_map(|line| {
                let (name, reason) = directive_target(line);
                let stage = self.stages.iter().position(|stage| stage.name == name)?;
                (stage > from && self.skippable(stage)).then(|| (stage, reason.to_owned()))
            })
            .collect()
    }

    /// List permitted return targets in workflow order.
    pub fn back_targets(&self, i: usize) -> Vec<usize> {
        let earlier = 0..i.min(self.stages.len());
        match self.stages.get(i) {
            Some(stage) if !stage.back_to.is_empty() => earlier
                .filter(|&j| stage.back_to.contains(&self.stages[j].name))
                .collect(),
            _ => earlier.collect(),
        }
    }

    /// Interpret the last AEP-BACK directive. Only known earlier stages can be rejected.
    pub fn route_back(&self, from: usize, result: &str) -> Back {
        let Some(line) = result
            .lines()
            .rev()
            .find_map(|line| line.trim().strip_prefix("AEP-BACK:"))
        else {
            return Back::None;
        };
        let (name, question) = directive_target(line);
        let Some(stage) = self.stages[..from.min(self.stages.len())]
            .iter()
            .rposition(|stage| stage.name == name)
        else {
            return Back::None;
        };
        let allowed = self.back_targets(from);
        if allowed.contains(&stage) {
            Back::To {
                stage,
                question: question.to_owned(),
            }
        } else {
            Back::Rejected {
                name: name.to_owned(),
                question: question.to_owned(),
                allowed: allowed
                    .into_iter()
                    .map(|i| self.stages[i].name.clone())
                    .collect(),
            }
        }
    }
}

fn directive_target(line: &str) -> (&str, &str) {
    let (name, reason) = line
        .split_once('：')
        .or_else(|| line.split_once(':'))
        .unwrap_or((line, ""));
    (
        name.trim().trim_matches(['「', '」', '<', '>']),
        reason.trim(),
    )
}

const BUILTIN_TEMPLATES: [Template; 3] = [Template::Legacy, Template::Software, Template::Game];

pub const fn template_id(template: Template) -> &'static str {
    match template {
        Template::Legacy => "Legacy",
        Template::Software => "Software",
        Template::Game => "Game",
    }
}

impl Template {
    pub fn label(self) -> &'static str {
        match self {
            Self::Legacy => "经典开发流程",
            Self::Software => "软件开发",
            Self::Game => "游戏开发",
        }
    }

    pub fn stages(self) -> &'static [FlowStage] {
        &builtin(self).stages
    }
}

pub fn builtin(template: Template) -> &'static Flow {
    static FLOWS: OnceLock<[Flow; 3]> = OnceLock::new();
    let flows = FLOWS.get_or_init(|| BUILTIN_TEMPLATES.map(make_builtin));
    match template {
        Template::Legacy => &flows[0],
        Template::Software => &flows[1],
        Template::Game => &flows[2],
    }
}

pub fn builtin_by_id(id: &str) -> Option<&'static Flow> {
    BUILTIN_TEMPLATES
        .into_iter()
        .find(|&template| template_id(template) == id)
        .map(builtin)
}

fn make_builtin(template: Template) -> Flow {
    let stages = match template {
        Template::Legacy => LEGACY,
        Template::Software => SOFTWARE,
        Template::Game => GAME,
    };
    Flow {
        id: template_id(template).into(),
        label: template.label().into(),
        stages: stages
            .iter()
            .enumerate()
            .map(|(i, &(name, brief, role))| FlowStage {
                name: name.into(),
                brief: brief.into(),
                role,
                skippable: i > 0 && role == Role::Plan,
                back_to: vec![],
            })
            .collect(),
    }
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Issue {
    pub stage: Option<usize>,
    pub field: &'static str,
    pub message: String,
}

#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub struct Validation {
    pub errors: Vec<Issue>,
    pub warnings: Vec<Issue>,
}

pub fn validate(flow: &Flow, taken_labels: &[&str]) -> Validation {
    let mut result = Validation::default();
    let errors = &mut result.errors;
    let label = flow.label.trim();
    let label_error = if label.is_empty() {
        Some("流程名称不能为空".to_owned())
    } else if taken_labels.iter().any(|taken| taken.trim() == label) {
        Some(if BUILTIN_TEMPLATES.iter().any(|t| t.label() == label) {
            "不能与内置流程同名".to_owned()
        } else {
            format!("与「{label}」重名")
        })
    } else {
        None
    };
    if let Some(message) = label_error {
        errors.push(Issue {
            stage: None,
            field: "label",
            message,
        });
    }

    let count_error = match flow.stages.len() {
        0..=1 => Some("至少需要 2 个阶段"),
        2..=20 => None,
        _ => Some("最多 20 个阶段"),
    };
    if let Some(message) = count_error {
        errors.push(Issue {
            stage: None,
            field: "stages",
            message: message.into(),
        });
    }

    let delivery = flow
        .stages
        .split_last()
        .filter(|(stage, _)| stage.role == Role::Deliver);
    if delivery.is_none_or(|(_, earlier)| earlier.iter().any(|s| s.role == Role::Deliver)) {
        errors.push(Issue {
            stage: None,
            field: "role",
            message: "需要恰好一个「交付」阶段，并放在最后".into(),
        });
    }

    let mut earlier_names = HashMap::new();
    for (i, stage) in flow.stages.iter().enumerate() {
        let n = i + 1;
        let name = stage.name.trim();
        let mut error = |field, message| {
            errors.push(Issue {
                stage: Some(i),
                field,
                message,
            });
        };
        if name.is_empty() {
            error("name", format!("第 {n} 个阶段：名称不能为空"));
        }
        if name.chars().count() > 20 {
            error("name", format!("第 {n} 个阶段：不超过 20 个字"));
        }
        if let Some(first) = earlier_names.get(name) {
            error("name", format!("第 {n} 个阶段：与第 {first} 个阶段重名"));
        }
        if name.contains(['\r', '\n', '：', ':', '「', '」', '<', '>']) {
            error(
                "name",
                format!("第 {n} 个阶段：不能包含「：」「:」「「」」「<」「>」或换行"),
            );
        }
        if stage.skippable && (i == 0 || stage.role != Role::Plan) {
            error(
                "skippable",
                format!("第 {n} 个阶段「{name}」：只有非首个的规划阶段可以跳过"),
            );
        }
        for target in &stage.back_to {
            let target = target.trim();
            if !earlier_names.contains_key(target) {
                error(
                    "back_to",
                    format!(
                        "第 {n} 个阶段「{name}」：只能退回到它之前的阶段（「{target}」在它之后或已删除）"
                    ),
                );
            }
        }
        earlier_names.entry(name).or_insert(n);
    }

    if let Some((deliver, earlier)) = delivery
        && let Some(build) = earlier.iter().rposition(|stage| stage.role == Role::Build)
        && !earlier[build + 1..]
            .iter()
            .any(|stage| stage.role == Role::Accept)
    {
        result.warnings.push(Issue {
            stage: Some(build),
            field: "role",
            message: format!(
                "「{}」之后、「{}」之前没有验收阶段。合并将直接使用最后一次通过的阶段提交。",
                earlier[build].name.trim(),
                deliver.name.trim(),
            ),
        });
    }
    result
}

use Role::{Accept, Build, Deliver, Plan, Test};

const LEGACY: &[(&str, &str, Role)] = &[
    ("需求定义", "记录用户目标、范围和验收标准", Plan),
    ("方案设计", "记录技术方案、涉及仓库和接口约定", Plan),
    ("开发实现", "跟进各仓库的实现任务和代码变更", Build),
    ("测试审查", "核对验收结果、审查意见和遗留问题", Test),
    ("发布归档", "记录发布版本、交付范围和证据位置", Deliver),
];

const SOFTWARE: &[(&str, &str, Role)] = &[
    (
        "产品需求",
        "整理 PRD，给用户场景、范围、边界情况和可验证的验收标准编号。",
        Plan,
    ),
    (
        "交互设计",
        "依据 PRD 描述用户路径、界面布局和交互状态，覆盖空数据、加载和失败。无界面改动时记录原因。",
        Plan,
    ),
    (
        "技术方案",
        "明确仓库边界、接口、数据、变更文件、兼容性与可执行的测试计划。",
        Plan,
    ),
    (
        "开发实现",
        "在事项工作区实现已确认的方案，记录变更文件和验证命令。",
        Build,
    ),
    (
        "测试验证",
        "运行相关测试，逐项核对验收标准，提供实际命令、结果及失败原因。",
        Test,
    ),
    (
        "独立验收",
        "独立检查实际变更及测试证据，逐项标注通过、失败或阻塞；缺少必要证据时给出阻塞。",
        Accept,
    ),
    (
        "交付归档",
        "汇总版本、变更范围、验收证据、遗留问题和人工交付步骤。",
        Deliver,
    ),
];

const GAME: &[(&str, &str, Role)] = &[
    (
        "玩法需求",
        "冻结玩家行为、目标平台和地区，给正常路径、异常路径及验收标准编号。",
        Plan,
    ),
    (
        "影响分析",
        "按数值表、场景与 LFS 资源、客户端、协议、服务端及打包拆分受影响的工作包。",
        Plan,
    ),
    (
        "环境预检",
        "实际检查引擎和导出模板、LFS、数据及设备可用性，列出缺失能力及所需人工证据。",
        Test,
    ),
    (
        "游戏实现",
        "在事项工作区完成涉及的玩法、资源或服务端工作包，保留可复核的变更。",
        Build,
    ),
    (
        "运行验证",
        "运行静态检查，再验证适用的场景、玩法、设备或构建，记录命令和运行证据。",
        Test,
    ),
    (
        "玩家验收",
        "逐条核对冻结的 AC，将结果标为通过、失败、阻塞或不适用，注明证据和不适用原因。",
        Accept,
    ),
    (
        "交付归档",
        "核对版本、平台、构建回执与设备证据，汇总人工交付步骤及剩余风险。",
        Deliver,
    ),
];
