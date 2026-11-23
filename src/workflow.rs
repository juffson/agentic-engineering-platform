//! Project workflows and the durable contract between Studio and its CLI supervisor.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, clap::ValueEnum)]
pub enum Template {
    #[default]
    Legacy,
    Software,
    Game,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Plan,
    Build,
    Test,
    Accept,
    Deliver,
}
pub struct Stage {
    pub name: &'static str,
    pub brief: &'static str,
    pub role: Role,
}
const fn stage(name: &'static str, brief: &'static str, role: Role) -> Stage {
    Stage { name, brief, role }
}
use Role::*;
impl Template {
    pub fn label(self) -> &'static str {
        match self {
            Self::Legacy => "经典开发流程",
            Self::Software => "软件开发",
            Self::Game => "游戏开发",
        }
    }
    pub fn stages(self) -> &'static [Stage] {
        match self {
            Self::Legacy => LEGACY,
            Self::Software => SOFTWARE,
            Self::Game => GAME,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, clap::ValueEnum)]
pub enum Client {
    Codex,
    Claude,
    #[value(name = "zcode")]
    ZCode,
    Custom,
}
impl Client {
    pub const ALL: [Self; 4] = [Self::Codex, Self::Claude, Self::ZCode, Self::Custom];
    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude",
            Self::ZCode => "ZCode",
            Self::Custom => "自定义 CLI",
        }
    }
    pub fn binary(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::ZCode => "zcode",
            Self::Custom => "",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Profile {
    pub client: Client,
    pub executable: String,
    pub model: String,
    #[serde(default)]
    pub check_command: String,
}
impl Profile {
    pub fn for_stage(s: &Stage) -> Self {
        Self {
            client: match s.role {
                Plan | Accept => Client::Claude,
                _ => Client::Codex,
            },
            executable: String::new(),
            model: String::new(),
            check_command: String::new(),
        }
    }
    pub fn program(&self, path: &std::ffi::OsStr) -> Result<PathBuf> {
        use std::os::unix::fs::PermissionsExt;
        let configured = self.executable.trim();
        let name = if configured.is_empty() {
            self.client.binary()
        } else {
            configured
        };
        ensure!(!name.is_empty(), "请填写可执行文件");
        let usable = |p: &PathBuf| {
            p.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        };
        if name.contains('/') {
            let p = if let Some(rel) = name.strip_prefix("~/") {
                std::env::home_dir().context("无法定位主目录")?.join(rel)
            } else {
                PathBuf::from(name)
            };
            ensure!(p.is_absolute() && usable(&p), "找不到可执行文件 {name}");
            return Ok(p);
        }
        std::env::split_paths(path)
            .map(|p| p.join(name))
            .chain(
                (configured.is_empty() && self.client == Client::ZCode)
                    .then(|| PathBuf::from(ZCODE_APP)),
            )
            .find(usable)
            .with_context(|| format!("未找到 {name}，请填写本机 CLI 路径"))
    }
    pub fn zcode_selection(&self) -> Result<serde_json::Value> {
        let (provider, rest) = self
            .model
            .split_once('/')
            .context("请使用 ZCode 的 provider/model 或 provider/model@推理档位")?;
        let (model, level) = rest
            .rsplit_once('@')
            .map_or((rest, None), |(m, l)| (m, Some(l)));
        ensure!(
            !provider.is_empty() && !model.is_empty() && level != Some(""),
            "ZCode 模型配置不完整"
        );
        let mut selection = serde_json::json!({"providerId":provider,"modelId":model});
        if let Some(level) = level {
            selection["options"] = serde_json::json!({"reasoningLevel":level});
        }
        Ok(selection)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.client != Client::Custom || !self.executable.trim().is_empty(),
            "请选择自定义 CLI 的可执行文件"
        );
        if !self.check_command.is_empty() {
            ensure!(
                self.client == Client::Claude
                    && self
                        .check_command
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b" ./_=-+:".contains(&b)),
                "验收命令需要是单条精确命令，支持程序、参数和路径"
            );
        }
        if self.client == Client::ZCode && !self.model.is_empty() {
            self.zcode_selection()?;
        }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExternalSession {
    pub session: String,
    pub workspace: PathBuf,
}
/// A message that continues an earlier attempt's native CLI conversation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FollowUp {
    /// The attempt whose session and workspace this turn continues.
    pub attempt: String,
    pub message: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Request {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up: Option<FollowUp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage_transfer: Option<crate::project::transfer::Transfer>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub handoffs: Vec<crate::project::WorkHandoff>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external: Option<ExternalSession>,
    #[serde(default)]
    pub prior_runs: Vec<(usize, String)>,
    pub project: String,
    pub work: String,
    pub template: Template,
    pub stage: usize,
    pub profile: Profile,
    pub repository: Option<PathBuf>,
    #[serde(default)]
    pub ticket: Option<String>,
    pub brief: String,
    pub approved: Vec<(String, String)>,
}
pub fn attempt_dir(catalog: &Path, project: &str, work: &str, attempt: &str) -> PathBuf {
    catalog
        .join("workflows")
        .join(project)
        .join(work)
        .join("runs")
        .join(attempt)
}
/// Page on UTF-8 character boundaries without loading the entire CLI transcript.
pub fn log_page(path: &Path, page: u64) -> std::io::Result<(String, u64)> {
    const SIZE: u64 = 3072;
    let mut file = fs::File::open(path)?;
    let pages = file.metadata()?.len().div_ceil(SIZE).max(1);
    file.seek(SeekFrom::Start(page.min(pages - 1) * SIZE))?;
    let mut bytes = Vec::new();
    file.take(SIZE + 3).read_to_end(&mut bytes)?;
    let start = bytes
        .iter()
        .position(|b| b & 0xc0 != 0x80)
        .unwrap_or(bytes.len());
    let mut end = (SIZE as usize).min(bytes.len());
    while end < bytes.len() && bytes[end] & 0xc0 == 0x80 {
        end += 1;
    }
    Ok((
        String::from_utf8_lossy(&bytes[start..end]).into_owned(),
        pages,
    ))
}
pub fn tail(path: &Path, limit: usize) -> String {
    (|| -> std::io::Result<String> {
        let mut f = fs::File::open(path)?;
        let len = f.metadata()?.len();
        f.seek(SeekFrom::Start(len.saturating_sub(limit as u64)))?;
        let mut buf = Vec::new();
        f.take(limit as u64).read_to_end(&mut buf)?;
        Ok(String::from_utf8_lossy(&buf).into_owned())
    })()
    .unwrap_or_default()
}
pub fn evidence(dir: &Path) -> Result<serde_json::Value> {
    let status = crate::workspace::status(
        dir.parent()
            .and_then(Path::parent)
            .context("invalid attempt directory")?,
        dir.file_name()
            .and_then(|s| s.to_str())
            .context("invalid attempt ID")?,
    )?;
    if status["status"] == "initializing"
        && dir
            .join("request.json")
            .metadata()?
            .modified()?
            .elapsed()
            .unwrap_or_default()
            > Duration::from_secs(30)
    {
        return Ok(serde_json::json!({"status":"unstarted","error":"执行器未启动，可以重试"}));
    }
    Ok(status)
}

const LEGACY: &[Stage] = &[
    stage("需求定义", "记录用户目标、范围和验收标准", Plan),
    stage("方案设计", "记录技术方案、涉及仓库和接口约定", Plan),
    stage("开发实现", "跟进各仓库的实现任务和代码变更", Build),
    stage("测试审查", "核对验收结果、审查意见和遗留问题", Test),
    stage("发布归档", "记录发布版本、交付范围和证据位置", Deliver),
];

const SOFTWARE: &[Stage] = &[
    stage(
        "产品需求",
        "整理 PRD，给用户场景、范围、边界情况和可验证的验收标准编号。",
        Plan,
    ),
    stage(
        "交互设计",
        "依据 PRD 描述用户路径、界面布局和交互状态，覆盖空数据、加载和失败。无界面改动时记录原因。",
        Plan,
    ),
    stage(
        "技术方案",
        "明确仓库边界、接口、数据、变更文件、兼容性与可执行的测试计划。",
        Plan,
    ),
    stage(
        "开发实现",
        "在事项工作区实现已确认的方案，记录变更文件和验证命令。",
        Build,
    ),
    stage(
        "测试验证",
        "运行相关测试，逐项核对验收标准，提供实际命令、结果及失败原因。",
        Test,
    ),
    stage(
        "独立验收",
        "独立检查实际变更及测试证据，逐项标注通过、失败或阻塞；缺少必要证据时给出阻塞。",
        Accept,
    ),
    stage(
        "交付归档",
        "汇总版本、变更范围、验收证据、遗留问题和人工交付步骤。",
        Deliver,
    ),
];

const GAME: &[Stage] = &[
    stage(
        "玩法需求",
        "冻结玩家行为、目标平台和地区，给正常路径、异常路径及验收标准编号。",
        Plan,
    ),
    stage(
        "影响分析",
        "按数值表、场景与 LFS 资源、客户端、协议、服务端及打包拆分受影响的工作包。",
        Plan,
    ),
    stage(
        "环境预检",
        "实际检查引擎和导出模板、LFS、数据及设备可用性，列出缺失能力及所需人工证据。",
        Test,
    ),
    stage(
        "游戏实现",
        "在事项工作区完成涉及的玩法、资源或服务端工作包，保留可复核的变更。",
        Build,
    ),
    stage(
        "运行验证",
        "运行静态检查，再验证适用的场景、玩法、设备或构建，记录命令和运行证据。",
        Test,
    ),
    stage(
        "玩家验收",
        "逐条核对冻结的 AC，将结果标为通过、失败、阻塞或不适用，注明证据和不适用原因。",
        Accept,
    ),
    stage(
        "交付归档",
        "核对版本、平台、构建回执与设备证据，汇总人工交付步骤及剩余风险。",
        Deliver,
    ),
];

pub const ZCODE_APP: &str = "/Applications/ZCode.app/Contents/Resources/glm/zcode.cjs";
