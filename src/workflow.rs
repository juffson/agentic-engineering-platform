//! Project workflows and the durable contract between Studio and its CLI supervisor.
pub use crate::flow::FlowStage as Stage;
use crate::flow::{Flow, builtin};
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
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Role {
    Plan,
    Build,
    Test,
    Accept,
    Deliver,
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
                Role::Plan | Role::Accept => Client::Claude,
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
/// How much a stage's CLI may touch beyond its workspace.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sandbox {
    /// Codex `read-only`/`workspace-write`; Claude asks before Bash.
    #[default]
    Standard,
    /// Codex workspace sandbox with network (local ports); Claude may run any Bash command.
    Network,
    /// No sandbox: Codex `danger-full-access`, Claude `bypassPermissions` (GUI, desktop services).
    Full,
}
impl Sandbox {
    pub const ALL: [Sandbox; 3] = [Sandbox::Standard, Sandbox::Network, Sandbox::Full];
    pub fn label(self) -> &'static str {
        match self {
            Self::Standard => "默认沙箱",
            Self::Network => "允许网络",
            Self::Full => "完全权限",
        }
    }
    /// Tests and acceptance need to run suites that listen on local ports.
    pub fn for_role(role: Role) -> Self {
        match role {
            Role::Test | Role::Accept => Self::Network,
            _ => Self::Standard,
        }
    }
}
/// One small piece of the build, planned in `task.json` → `subtasks` by the design stage.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Subtask {
    pub id: String,
    #[serde(default)]
    pub title: String,
    /// Files or directories the subtask may change.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Acceptance items it covers.
    #[serde(default)]
    pub criteria: Vec<String>,
    /// Command (argv) that verifies it on its own.
    #[serde(default)]
    pub check: Vec<String>,
}
/// A build subtask that already passed: the next subtask continues from its session and
/// reads its hand-off line, so it does not have to rediscover what was done.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DoneSubtask {
    pub id: String,
    #[serde(default)]
    pub title: String,
    /// The attempt that passed it.
    pub run: String,
    /// Its AEP-HANDOFF line (or the start of its answer).
    #[serde(default)]
    pub summary: String,
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
    #[serde(default)]
    pub sandbox: Sandbox,
    /// Set when the build runs one planned subtask per session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtask: Option<Subtask>,
    /// Subtasks of this build already passed, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subtasks_done: Vec<DoneSubtask>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up: Option<FollowUp>,
    /// Tokens this attempt may use before it is stopped (what is left of the budgets).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_limit: Option<u64>,
    /// Context above which a session is not continued (R2); older requests use the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_limit: Option<u64>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<Flow>,
    pub stage: usize,
    pub profile: Profile,
    pub repository: Option<PathBuf>,
    #[serde(default)]
    pub ticket: Option<String>,
    pub brief: String,
    pub approved: Vec<(String, String)>,
}
impl Request {
    /// Older execution requests identify their flow by template alone.
    pub fn flow(&self) -> &Flow {
        self.flow.as_ref().unwrap_or_else(|| builtin(self.template))
    }
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

pub const ZCODE_APP: &str = "/Applications/ZCode.app/Contents/Resources/glm/zcode.cjs";

/// Models to offer for a CLI. For Codex this is the list the installed CLI last fetched for
/// this account (`models_cache.json`), so it never suggests a model the CLI cannot use.
pub fn suggested_models(client: Client) -> Vec<String> {
    match client {
        Client::Codex => {
            let home = std::env::var_os("CODEX_HOME")
                .map(std::path::PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".codex"))
                });
            let listed: Vec<String> = home
                .and_then(|h| std::fs::read(h.join("models_cache.json")).ok())
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                .and_then(|v| v["models"].as_array().cloned())
                .unwrap_or_default()
                .iter()
                .filter(|m| m["visibility"] == "list")
                .filter_map(|m| m["slug"].as_str().map(str::to_owned))
                .take(4)
                .collect();
            if listed.is_empty() {
                vec!["gpt-6.1-sol".into()]
            } else {
                listed
            }
        }
        Client::Claude => vec!["sonnet".into(), "opus".into()],
        _ => vec![],
    }
}
