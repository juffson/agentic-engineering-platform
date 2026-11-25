pub mod archive;
pub mod branches;
pub mod directory;
pub mod import;
pub mod merge;
pub mod shelf;
pub mod timeline;
pub mod transfer;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};
use uuid::Uuid;

use crate::{
    flow::{self, Flow, FlowStage, Issue},
    workflow::{self, Client, Profile, Template},
};
use std::collections::BTreeMap;

pub fn catalog_dir() -> Option<PathBuf> {
    std::env::var_os("AEP_PROJECT_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::home_dir().map(|home| {
                if cfg!(target_os = "macos") {
                    home.join("Library/Application Support/AEP Studio")
                } else {
                    home.join(".local/share/aep-studio")
                }
            })
        })
}
pub fn save_json(dir: &Path, name: &str, value: &impl Serialize) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.as_file().sync_all()?;
    file.persist(dir.join(name))?;
    Ok(())
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Attempt {
    pub id: String,
    pub stage: usize,
    pub profile: Profile,
    pub repository: Option<PathBuf>,
}
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Kind {
    Feature,
    Release,
}
impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Feature => "功能",
            Self::Release => "版本",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpsSite {
    pub endpoint: String,
    pub ca: Option<PathBuf>,
    pub operator_file: PathBuf,
}

impl HttpsSite {
    pub fn validate(&self) -> Result<()> {
        let origin = url::Url::parse(&self.endpoint).context("请填写有效的控制面地址")?;
        ensure!(
            origin.scheme() == "https"
                && origin.host().is_some()
                && origin.username().is_empty()
                && origin.password().is_none()
                && origin.query().is_none()
                && origin.fragment().is_none()
                && origin.path() == "/",
            "请填写 HTTPS 控制面地址，只包含主机和可选端口"
        );
        ensure!(
            self.operator_file.is_absolute() && self.operator_file.is_file(),
            "请选择操作员令牌文件的绝对路径"
        );
        ensure!(
            self.ca
                .as_ref()
                .is_none_or(|p| p.is_absolute() && p.is_file()),
            "请填写 CA 证书文件的绝对路径"
        );
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum RecordKind {
    Run,
    Relay,
    Https,
}

impl RecordKind {
    /// Read only the linked record, without exposing connection fields or read errors.
    pub fn summary(self, repository: &Path, id: &str) -> serde_json::Value {
        use serde_json::json;

        #[derive(Deserialize)]
        struct Name {
            name: String,
        }
        #[derive(Deserialize)]
        struct Request {
            task: Name,
            branch: Name,
            ticket: Option<String>,
            endpoint: Option<String>,
            ca: Option<String>,
            operator_file: Option<String>,
        }
        #[derive(Default, Deserialize)]
        struct State {
            stage: Option<String>,
            exit_code: Option<i32>,
            pr: Option<u64>,
            reason: Option<String>,
        }
        // Keep each record and its JSON files at their own archive paths.
        fn confined(parent: &Path, name: &str) -> std::result::Result<PathBuf, &'static str> {
            let path = parent.join(name);
            if fs::symlink_metadata(&path)
                .map_err(|_| "missing")?
                .is_symlink()
            {
                return Err("corrupt");
            }
            Ok(path)
        }
        fn read<T: serde::de::DeserializeOwned>(
            path: &Path,
        ) -> std::result::Result<T, &'static str> {
            if !fs::symlink_metadata(path).map_err(|_| "missing")?.is_file() {
                return Err("corrupt");
            }
            let file = fs::File::open(path).map_err(|_| "missing")?;
            serde_json::from_reader(std::io::BufReader::new(file)).map_err(|_| "corrupt")
        }
        let summarize = || -> std::result::Result<serde_json::Value, &'static str> {
            if self != Self::Https
                || !id.starts_with("studio-")
                || id.len() > 100
                || !id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
            {
                return Err("corrupt");
            }
            let ws = crate::workspace::Workspace::open(repository).map_err(|_| "missing")?;
            if fs::symlink_metadata(&ws.data)
                .map_err(|_| "missing")?
                .is_symlink()
            {
                return Err("corrupt");
            }
            let parent = confined(&ws.data, "https-studio")?;
            let dir = confined(&parent, id)?;
            let req: Request = read(&confined(&dir, "request.json")?)?;
            let mut summary = json!({"id":id,"repository":repository,
                "task":req.task.name,"branch":req.branch.name,"ticket":req.ticket});
            // Only an absent state is optional; unreadable or malformed state is an error.
            match fs::symlink_metadata(dir.join("state.json")) {
                Err(e) if e.kind() == ErrorKind::NotFound => return Ok(summary),
                Err(_) => return Err("missing"),
                Ok(_) => {}
            }
            let state: State = read(&confined(&dir, "state.json")?)?;
            let reason = state.reason.map(|reason| {
                let site = req
                    .endpoint
                    .as_deref()
                    .and_then(|s| url::Url::parse(s).ok());
                let sensitive = [
                    req.endpoint.as_deref(),
                    site.as_ref().and_then(|s| s.host_str()),
                    req.ca.as_deref(),
                    req.operator_file.as_deref(),
                ]
                .into_iter()
                .flatten()
                .any(|s| !s.is_empty() && reason.contains(s));
                if sensitive {
                    "已隐藏".to_owned()
                } else {
                    reason.chars().take(200).collect()
                }
            });
            summary["stage"] = json!(state.stage);
            summary["exit_code"] = json!(state.exit_code);
            summary["pr"] = json!(state.pr);
            summary["reason"] = json!(reason);
            Ok(summary)
        };
        summarize().unwrap_or_else(|error| json!({"id":id,"error":error}))
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Relay => "relay",
            Self::Https => "https",
        }
    }

    pub fn verify(self, ws: &crate::workspace::Workspace, id: &str) -> Result<()> {
        ensure!(
            !id.is_empty()
                && id.len() <= 100
                && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'),
            "记录 ID 无效"
        );
        let (prefix, dir, file) = match self {
            Self::Run => {
                crate::workspace::ident(id, 64)?;
                let record =
                    crate::workspace::read(&ws.data.join("runs").join(id).join("evidence.json"))?;
                ensure!(record["run_id"] == id, "运行回执与记录不匹配");
                ensure!(
                    timeline::relay_for_run(ws, id)?.is_none(),
                    "请关联此运行所属的两轮接力"
                );
                return Ok(());
            }
            Self::Relay => ("relay-", "relays", "relay.json"),
            Self::Https => ("studio-", "https-studio", "request.json"),
        };
        ensure!(id.starts_with(prefix), "记录 ID 与类型不匹配");
        let record = crate::workspace::read(&ws.data.join(dir).join(id).join(file))
            .context("无法读取此仓库中的执行记录")?;
        ensure!(
            record["task"]["name"]
                .as_str()
                .is_some_and(|name| !name.is_empty()),
            "记录缺少任务名称"
        );
        match self {
            Self::Run => unreachable!(),
            Self::Relay => ensure!(
                record["id"] == id && record["attempts"].is_array(),
                "接力记录身份或轮次无效"
            ),
            Self::Https => ensure!(
                record["branch"]["name"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
                    && serde_json::from_value::<HttpsSite>(record).is_ok(),
                "HTTPS 任务缺少目标分支或连接信息"
            ),
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct WorkRecord {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub session_stages: BTreeMap<String, usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<usize>,
    pub repository: PathBuf,
    pub kind: RecordKind,
    pub id: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct WorkHandoff {
    pub from: String,
    pub name: String,
    pub stage: String,
    pub note: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct WorkFlow {
    #[serde(flatten)]
    pub flow: Flow,
    pub source_revision: u64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct BrokenWork {
    pub error: String,
    pub template: Value,
    pub flow: Value,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Work {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deliveries: Vec<archive::Delivery>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stage_transfers: Vec<transfer::Transfer>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub active_transfers: BTreeMap<usize, usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<WorkRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub handoffs: Vec<WorkHandoff>,
    /// Stages to pass over, with the reason (a person's choice or an earlier stage's AEP-SKIP).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub skipped: BTreeMap<usize, String>,
    /// The existing branch this item was imported from, for re-import and sync.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported: Option<import::Imported>,
    /// Per-stage sandbox choice; unset stages use the role default.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sandboxes: BTreeMap<usize, workflow::Sandbox>,
    /// Each time a later stage sent the item back for rework.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reworks: Vec<transfer::Rework>,
    /// Set once a delivered item is put away.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shelved: Option<shelf::Shelved>,
    /// Token budget per stage (every attempt of the stage counts); unset stages use the
    /// project default.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub budgets: BTreeMap<usize, u64>,
    /// Token budget for all stages together.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_total: Option<u64>,
    #[serde(default)]
    pub tickets: std::collections::BTreeMap<PathBuf, String>,
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub stage: usize,
    pub notes: Vec<String>,
    #[serde(default)]
    pub template: Template,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<WorkFlow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_error: Option<BrokenWork>,
    #[serde(default)]
    pub profiles: BTreeMap<usize, Profile>,
    #[serde(default)]
    pub targets: BTreeMap<usize, PathBuf>,
    #[serde(default)]
    pub attempts: Vec<Attempt>,
    #[serde(default)]
    pub current_attempt: BTreeMap<usize, String>,
    pub repositories: Vec<PathBuf>,
}
impl Work {
    pub fn flow(&self) -> Result<&Flow> {
        if let Some(broken) = &self.flow_error {
            anyhow::bail!("这个事项的流程数据无法读取：{}", broken.error);
        }
        Ok(self
            .flow
            .as_ref()
            .map_or_else(|| flow::builtin(self.template), |snapshot| &snapshot.flow))
    }

    pub fn stages(&self) -> &[FlowStage] {
        self.flow().map_or(&[], |flow| flow.stages.as_slice())
    }

    pub fn flow_id(&self) -> &str {
        self.flow().map_or("", |flow| flow.id.as_str())
    }

    pub fn has_record(&self, repo: &Path, kind: RecordKind, id: &str) -> bool {
        self.records
            .iter()
            .any(|r| r.repository == repo && r.kind == kind && r.id == id)
    }
    pub fn unlink_record(&mut self, repo: &Path, kind: RecordKind, id: &str) -> Result<()> {
        ensure!(self.has_record(repo, kind, id), "事项未关联此记录");
        self.records
            .retain(|r| r.repository != repo || r.kind != kind || r.id != id);
        Ok(())
    }
    /// Only optional planning stages after the first may be skipped; gates never are.
    pub fn skippable(&self, stage: usize) -> bool {
        self.flow().is_ok_and(|flow| flow.skippable(stage))
    }
    pub fn sandbox(&self, stage: usize) -> workflow::Sandbox {
        self.sandboxes.get(&stage).copied().unwrap_or_else(|| {
            self.stages()
                .get(stage)
                .map_or_else(Default::default, |s| workflow::Sandbox::for_role(s.role))
        })
    }
    pub fn stage_name(&self) -> &str {
        self.flow().map_or("流程无法读取", |flow| {
            flow.stages
                .get(self.stage)
                .map_or("已交付", |stage| stage.name.as_str())
        })
    }
    pub fn profile(&self, stage: usize) -> Profile {
        let Some(definition) = self.stages().get(stage) else {
            return Profile {
                client: Client::Codex,
                executable: String::new(),
                model: String::new(),
                check_command: String::new(),
            };
        };
        self.profiles
            .get(&stage)
            .cloned()
            .unwrap_or_else(|| Profile::for_stage(definition))
    }
    pub fn target(&self, stage: usize) -> Option<PathBuf> {
        self.targets
            .get(&stage)
            .filter(|p| self.repositories.contains(p))
            .or_else(|| self.repositories.first())
            .cloned()
    }
    pub fn include(&mut self, path: &Path) {
        if !self.repositories.iter().any(|repo| repo == path) {
            self.repositories.push(path.to_owned());
            self.tickets
                .entry(path.to_owned())
                .or_insert_with(|| format!("work-{}", Uuid::new_v4().simple()));
        }
    }
    pub fn ticket(&self, repo: &Path) -> String {
        self.tickets
            .get(repo)
            .cloned()
            .unwrap_or_else(|| format!("work-{}", self.id))
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ProjectFlow {
    #[serde(flatten)]
    pub flow: Flow,
    pub revision: u64,
    pub updated_at: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Broken {
    pub error: String,
    pub raw: Value,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Project {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub https_sites: BTreeMap<PathBuf, HttpsSite>,
    pub id: String,
    pub name: String,
    pub repositories: Vec<PathBuf>,
    pub work: Vec<Work>,
    pub selected: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flows: Vec<ProjectFlow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub broken_flows: Vec<Broken>,
    #[serde(default)]
    pub cli_defaults: Vec<Profile>,
    /// Branches whose imported item a person deleted; sync does not bring them back.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dismissed_imports: Vec<String>,
    /// Default token budget for a stage that has none of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage_budget: Option<u64>,
    /// Last-call context above which a stage session is not continued (R2); None = default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_limit: Option<u64>,
}
impl Project {
    pub fn flow_choices(&self) -> Vec<&Flow> {
        let mut custom: Vec<_> = self.flows.iter().map(|entry| &entry.flow).collect();
        custom.sort_by(|a, b| a.label.cmp(&b.label));
        [
            flow::builtin(Template::Software),
            flow::builtin(Template::Game),
        ]
        .into_iter()
        .chain(custom)
        .collect()
    }

    pub fn save_flow(&mut self, flow: Flow) -> Result<(String, Vec<Issue>)> {
        let mut flow = flow.normalized();
        ensure!(flow::builtin_by_id(&flow.id).is_none(), "内置流程不能修改");
        let existing = if flow.id.is_empty() {
            None
        } else {
            Some(
                self.flows
                    .iter()
                    .position(|entry| entry.flow.id == flow.id)
                    .context("这套流程已被删除")?,
            )
        };
        let warnings = validate_flow(&flow, &taken_flow_labels(&self.flows, &flow.id))?;
        let revision = if let Some(index) = existing {
            self.flows[index]
                .revision
                .checked_add(1)
                .context("流程版本号已达上限")?
        } else {
            flow.id = self.new_flow_id();
            1
        };
        let id = flow.id.clone();
        let saved = ProjectFlow {
            flow,
            revision,
            updated_at: chrono::Utc::now().to_rfc3339(),
        };
        if let Some(index) = existing {
            self.flows[index] = saved;
        } else {
            self.flows.push(saved);
        }
        Ok((id, warnings))
    }

    pub fn copy_flow(&mut self, id: &str, label: Option<&str>) -> Result<String> {
        let mut copy = flow::builtin_by_id(id)
            .or_else(|| {
                self.flows
                    .iter()
                    .find(|entry| entry.flow.id == id)
                    .map(|entry| &entry.flow)
            })
            .context("这套流程已被删除")?
            .clone();
        copy.id.clear();
        copy.label = match label {
            Some(label) => label.into(),
            None => {
                let labels = taken_flow_labels(&self.flows, "");
                let base = format!("{} 副本", copy.label);
                let mut label = base.clone();
                let mut suffix = 2;
                while labels.iter().any(|taken| taken.trim() == label) {
                    label = format!("{base} {suffix}");
                    suffix += 1;
                }
                label
            }
        };
        self.save_flow(copy).map(|(id, _)| id)
    }

    pub fn delete_flow(&mut self, id: &str) -> Result<usize> {
        ensure!(flow::builtin_by_id(id).is_none(), "内置流程不能删除");
        let count = self.flows.len() + self.broken_flows.len();
        self.flows.retain(|entry| entry.flow.id != id);
        self.broken_flows
            .retain(|entry| entry.raw["id"].as_str() != Some(id));
        ensure!(
            self.flows.len() + self.broken_flows.len() < count,
            "这套流程已被删除"
        );
        Ok(self.flow_in_use(id))
    }

    pub fn flow_in_use(&self, id: &str) -> usize {
        self.work
            .iter()
            .filter(|work| work.shelved.is_none() && work.flow().is_ok_and(|flow| flow.id == id))
            .count()
    }

    fn new_flow_id(&self) -> String {
        loop {
            let uuid = Uuid::new_v4().simple().to_string();
            let id = format!("flow-{}", &uuid[..12]);
            if !self.flows.iter().any(|entry| entry.flow.id == id)
                && !self
                    .broken_flows
                    .iter()
                    .any(|entry| entry.raw["id"].as_str() == Some(&id))
                && !self.work.iter().any(|work| work.flow_id() == id)
            {
                return id;
            }
        }
    }

    pub fn configure_site(&mut self, repo: &Path, site: Option<HttpsSite>) -> Result<()> {
        ensure!(
            self.repositories.iter().any(|r| r == repo),
            "仓库未关联到此项目"
        );
        if let Some(site) = site {
            site.validate()?;
            self.https_sites.insert(repo.into(), site);
        } else {
            self.https_sites.remove(repo);
        }
        Ok(())
    }

    pub fn link_record(
        &mut self,
        work: &str,
        repo: &Path,
        kind: RecordKind,
        id: &str,
    ) -> Result<()> {
        self.link_at_stage(work, repo, kind, id, None)
    }

    pub fn link_at_stage(
        &mut self,
        work: &str,
        repo: &Path,
        kind: RecordKind,
        id: &str,
        stage: Option<usize>,
    ) -> Result<()> {
        ensure!(
            !self
                .work
                .iter()
                .any(|w| w.id != work && w.has_record(repo, kind, id)),
            "该记录已关联本项目的另一事项"
        );
        let item = self.work_by_id_mut(work)?;
        ensure!(
            item.repositories.iter().any(|r| r == repo),
            "仓库未纳入当前事项"
        );
        ensure!(stage.is_none_or(|i| i < item.stages().len()), "无效阶段");
        if let Some(record) = item
            .records
            .iter_mut()
            .find(|r| r.repository == repo && r.kind == kind && r.id == id)
        {
            if stage.is_some() {
                record.stage = stage;
            }
        } else {
            item.records.push(WorkRecord {
                session_stages: BTreeMap::new(),
                stage,
                repository: repo.into(),
                kind,
                id: id.into(),
            });
        }
        Ok(())
    }

    pub fn link_session(
        &mut self,
        work: &str,
        repo: &Path,
        kind: RecordKind,
        record: &str,
        session: &str,
        stage: usize,
    ) -> Result<()> {
        let item = self.work_by_id_mut(work)?;
        ensure!(stage < item.stages().len(), "无效阶段");
        self.link_record(work, repo, kind, record)?;
        let item = self.work_by_id_mut(work)?;
        let link = item
            .records
            .iter_mut()
            .find(|r| r.kind == kind && r.repository == repo && r.id == record)
            .context("记录未关联")?;
        link.session_stages.insert(session.to_owned(), stage);
        Ok(())
    }

    pub fn handoff(&mut self, from: &str, to: &str, note: &str) -> Result<()> {
        let note = note.trim();
        ensure!(from != to, "请选择另一开发事项");
        ensure!(
            !note.is_empty() && note.chars().count() <= 500,
            "请填写 1 至 500 字的交接摘要"
        );
        let source = self.work_by_id_mut(from)?;
        let entry = WorkHandoff {
            from: from.into(),
            name: source.name.clone(),
            stage: source.stage_name().into(),
            note: note.into(),
        };
        let target = self.work_by_id_mut(to)?;
        target.handoffs.retain(|h| h.from != from);
        target.handoffs.push(entry);
        Ok(())
    }

    pub fn selected(&self) -> Option<&Work> {
        self.work
            .iter()
            .find(|w| Some(&w.id) == self.selected.as_ref())
    }
    pub fn selected_mut(&mut self) -> Result<&mut Work> {
        self.work
            .iter_mut()
            .find(|w| Some(&w.id) == self.selected.as_ref())
            .context("请选择开发事项")
    }
    pub fn zcode_default(&self) -> &str {
        self.preset(Client::ZCode).map_or("", |p| p.model.as_str())
    }
    pub fn set_zcode_default(&mut self, model: String) {
        if let Some(profile) = self
            .cli_defaults
            .iter_mut()
            .find(|p| p.client == Client::ZCode)
        {
            profile.model = model;
        } else if !model.is_empty() {
            self.cli_defaults.push(Profile {
                client: Client::ZCode,
                model,
                executable: String::new(),
                check_command: String::new(),
            });
        }
    }
    pub fn preset(&self, client: Client) -> Option<&Profile> {
        self.cli_defaults.iter().find(|p| p.client == client)
    }
    pub fn add_work(&mut self, name: &str, kind: Kind, template: Template) -> Result<()> {
        self.add_work_flow(name, kind, flow::template_id(template))
    }

    pub fn add_work_flow(&mut self, name: &str, kind: Kind, flow_id: &str) -> Result<()> {
        let name = name.trim();
        ensure!(!name.is_empty(), "请填写事项名称");
        let (template, flow) = if let Some(template) = flow::template_by_id(flow_id) {
            (template, None)
        } else {
            let source = self
                .flows
                .iter()
                .find(|entry| entry.flow.id == flow_id)
                .with_context(|| format!("流程「{flow_id}」已被删除，请重新选择"))?;
            (
                Template::Software,
                Some(WorkFlow {
                    flow: source.flow.clone(),
                    source_revision: source.revision,
                }),
            )
        };
        let id = Uuid::new_v4().simple().to_string();
        let mut work = Work {
            skipped: BTreeMap::new(),
            imported: None,
            sandboxes: BTreeMap::new(),
            reworks: vec![],
            shelved: None,
            budgets: BTreeMap::new(),
            budget_total: None,
            deliveries: vec![],
            stage_transfers: vec![],
            active_transfers: BTreeMap::new(),
            records: vec![],
            handoffs: vec![],
            tickets: Default::default(),
            id,
            name: name.into(),
            kind,
            stage: 0,
            notes: Vec::new(),
            template,
            flow,
            flow_error: None,
            profiles: BTreeMap::new(),
            targets: BTreeMap::new(),
            attempts: vec![],
            current_attempt: BTreeMap::new(),
            repositories: Vec::new(),
        };
        work.notes.resize(work.stages().len(), String::new());
        work.profiles = work
            .stages()
            .iter()
            .enumerate()
            .filter_map(|(i, stage)| {
                self.preset(Profile::for_stage(stage).client)
                    .cloned()
                    .map(|profile| (i, profile))
            })
            .collect();
        for path in &self.repositories {
            work.include(path);
        }
        self.selected = Some(work.id.clone());
        self.work.push(work);
        Ok(())
    }
}

fn taken_flow_labels<'a>(flows: &'a [ProjectFlow], excluded_id: &str) -> Vec<&'a str> {
    let mut labels: Vec<&'a str> = flow::BUILTIN_TEMPLATES.map(Template::label).to_vec();
    labels.extend(
        flows
            .iter()
            .filter(|entry| entry.flow.id != excluded_id)
            .map(|entry| entry.flow.label.as_str()),
    );
    labels
}

fn validate_flow(flow: &Flow, taken_labels: &[&str]) -> Result<Vec<Issue>> {
    let result = flow::validate(flow, taken_labels);
    ensure!(
        result.errors.is_empty(),
        "{}",
        result
            .errors
            .iter()
            .map(|issue| issue.message.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    Ok(result.warnings)
}

fn isolate_project_flows(project: &mut Map<String, Value>) -> Result<()> {
    let Some(raw_flows) = project.remove("flows") else {
        return Ok(());
    };
    let mut broken = Vec::new();
    let entries = match raw_flows {
        Value::Array(entries) => entries,
        raw => {
            broken.push(json!({"error": "项目流程列表无法读取", "raw": raw}));
            Vec::new()
        }
    };
    let mut parsed: Vec<ProjectFlow> = Vec::new();
    let mut valid = Vec::new();
    for raw in entries {
        match read_project_flow(&raw, &parsed) {
            Ok(entry) => {
                parsed.push(entry);
                valid.push(raw);
            }
            Err(error) => broken.push(json!({"error": format!("{error:#}"), "raw": raw})),
        }
    }
    project.insert("flows".into(), Value::Array(valid));
    if !broken.is_empty() {
        project
            .entry("broken_flows")
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .context("损坏的项目流程列表无法读取")?
            .extend(broken);
    }
    Ok(())
}

fn read_project_flow(raw: &Value, existing: &[ProjectFlow]) -> Result<ProjectFlow> {
    let entry = ProjectFlow::deserialize(raw).context("项目流程无法读取")?;
    let id = &entry.flow.id;
    ensure!(!id.is_empty(), "流程标识不能为空");
    ensure!(flow::builtin_by_id(id).is_none(), "内置流程不能修改");
    ensure!(
        !existing.iter().any(|valid| valid.flow.id == *id),
        "流程标识「{id}」重复"
    );
    validate_flow(&entry.flow, &taken_flow_labels(existing, ""))?;
    Ok(entry)
}

fn isolate_work_flow(work: &mut Map<String, Value>) {
    if let Err(error) = check_work_flow(work) {
        let template = work
            .insert("template".into(), json!(Template::Legacy))
            .unwrap_or(Value::Null);
        let flow = work.remove("flow").unwrap_or(Value::Null);
        work.insert(
            "flow_error".into(),
            json!({"error": format!("{error:#}"), "template": template, "flow": flow}),
        );
    }
}

fn check_work_flow(work: &Map<String, Value>) -> Result<()> {
    if let Some(template) = work.get("template") {
        Template::deserialize(template).context("事项模板无法识别")?;
    }
    if let Some(flow) = work.get("flow") {
        let snapshot = WorkFlow::deserialize(flow).context("事项流程快照无法读取")?;
        validate_flow(&snapshot.flow, &[])?;
    }
    Ok(())
}

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct Library {
    pub revision: u64,
    pub active: Option<String>,
    pub projects: Vec<Project>,
}
impl Library {
    pub fn open(dir: &Path) -> Result<Self> {
        match fs::read(dir.join("projects.json")) {
            Ok(data) => {
                let mut library = Self::decode(&data).context("项目库文件无法读取")?;
                for project in &mut library.projects {
                    for work in &mut project.work {
                        for delivery in &mut work.deliveries {
                            delivery.merge =
                                merge::receipt(dir, delivery)?.or(delivery.merge.take());
                        }
                    }
                }
                Ok(library)
            }
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    fn decode(data: &[u8]) -> Result<Self> {
        let mut raw: Value = serde_json::from_slice(data)?;
        if let Some(projects) = raw.get_mut("projects").and_then(Value::as_array_mut) {
            for project in projects.iter_mut().filter_map(Value::as_object_mut) {
                isolate_project_flows(project)?;
                if let Some(work) = project.get_mut("work").and_then(Value::as_array_mut) {
                    for item in work.iter_mut().filter_map(Value::as_object_mut) {
                        isolate_work_flow(item);
                    }
                }
            }
        }
        Ok(serde_json::from_value(raw)?)
    }

    pub fn project(&self) -> Option<&Project> {
        self.projects
            .iter()
            .find(|p| Some(&p.id) == self.active.as_ref())
    }
    pub fn project_mut(&mut self) -> Result<&mut Project> {
        self.projects
            .iter_mut()
            .find(|p| Some(&p.id) == self.active.as_ref())
            .context("请先打开项目")
    }
    pub fn create(&mut self, name: &str, repositories: Vec<PathBuf>) -> Result<()> {
        let name = name.trim();
        ensure!(!name.is_empty(), "请填写项目名称");
        ensure!(
            !self.projects.iter().any(|p| p.name == name),
            "项目名称已存在"
        );
        let id = Uuid::new_v4().simple().to_string();
        self.projects.push(Project {
            https_sites: BTreeMap::new(),
            id: id.clone(),
            name: name.into(),
            repositories,
            work: vec![],
            selected: None,
            flows: vec![],
            broken_flows: vec![],
            cli_defaults: vec![],
            dismissed_imports: vec![],
            stage_budget: None,
            resume_limit: None,
        });
        self.active = Some(id);
        Ok(())
    }
    pub fn change<T>(
        &mut self,
        dir: &Path,
        edit: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        fs::create_dir_all(dir)?;
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("projects.lock"))?;
        lock.lock().context("无法锁定项目库")?;
        let mut next = Self::open(dir)?;
        if self.revision != next.revision {
            *self = next;
            anyhow::bail!("项目已在另一窗口更新，已重新载入，请核对后重试");
        }
        let value = edit(&mut next)?;
        next.revision += 1;
        save_json(dir, "projects.json", &next)?;
        *self = next;
        lock.unlock()?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_five_stage_work_keeps_its_original_phase_and_notes() -> Result<()> {
        let old = serde_json::json!({"id":"legacy","name":"旧版本","stage":2,"kind":"Feature","notes":["需求","方案","","",""],"repositories":[]});
        let item: Work = serde_json::from_value(old)?;
        assert_eq!(item.template, Template::Legacy);
        assert_eq!(item.stage_name(), "开发实现");
        assert_eq!(item.notes[1], "方案");
        assert_eq!(item.profile(2).client, Client::Codex);
        Ok(())
    }
    #[test]
    fn parallel_windows_cannot_replace_a_newer_project() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let dir = temp.path();
        let mut first = Library::open(dir)?;
        let mut other = Library::open(dir)?;
        first.change(dir, |lib| lib.create("Studio", vec![]))?;
        assert!(
            other
                .change(dir, |lib| lib.create("Worker", vec![]))
                .is_err()
        );
        assert_eq!(
            other
                .project()
                .context("stale window did not reload the project")?
                .name,
            "Studio"
        );
        other.change(dir, |lib| lib.create("Worker", vec![]))?;
        assert_eq!(Library::open(dir)?.projects.len(), 2);
        fs::write(dir.join("projects.json"), b"damaged")?;
        assert!(
            other
                .change(dir, |lib| lib.create("Replacement", vec![]))
                .is_err()
        );
        assert_eq!(fs::read(dir.join("projects.json"))?, b"damaged");
        Ok(())
    }
}
