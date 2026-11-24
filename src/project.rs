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
use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};
use uuid::Uuid;

use crate::workflow::{self, Client, Profile, Template};
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
    /// Per-stage sandbox choice; unset stages use the role default.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sandboxes: BTreeMap<usize, workflow::Sandbox>,
    /// Each time a later stage sent the item back for rework.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reworks: Vec<transfer::Rework>,
    /// Set once a delivered item is put away.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shelved: Option<shelf::Shelved>,
    #[serde(default)]
    pub tickets: std::collections::BTreeMap<PathBuf, String>,
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub stage: usize,
    pub notes: Vec<String>,
    #[serde(default)]
    pub template: Template,
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
        workflow::skippable(self.template, stage)
    }
    pub fn sandbox(&self, stage: usize) -> workflow::Sandbox {
        self.sandboxes.get(&stage).copied().unwrap_or_else(|| {
            self.template
                .stages()
                .get(stage)
                .map_or_else(Default::default, |s| workflow::Sandbox::for_role(s.role))
        })
    }
    pub fn stage_name(&self) -> &'static str {
        self.template
            .stages()
            .get(self.stage)
            .map_or("已交付", |s| s.name)
    }
    pub fn profile(&self, stage: usize) -> Profile {
        self.profiles
            .get(&stage)
            .cloned()
            .unwrap_or_else(|| Profile::for_stage(&self.template.stages()[stage]))
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
pub struct Project {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub https_sites: BTreeMap<PathBuf, HttpsSite>,
    pub id: String,
    pub name: String,
    pub repositories: Vec<PathBuf>,
    pub work: Vec<Work>,
    pub selected: Option<String>,
    #[serde(default)]
    pub cli_defaults: Vec<Profile>,
}
impl Project {
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
        ensure!(
            stage.is_none_or(|i| i < item.template.stages().len()),
            "无效阶段"
        );
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
        ensure!(stage < item.template.stages().len(), "无效阶段");
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
        ensure!(!name.trim().is_empty(), "请填写事项名称");
        let id = Uuid::new_v4().simple().to_string();
        self.selected = Some(id.clone());
        let mut work = Work {
            skipped: BTreeMap::new(),
            sandboxes: BTreeMap::new(),
            reworks: vec![],
            shelved: None,
            deliveries: vec![],
            stage_transfers: vec![],
            active_transfers: BTreeMap::new(),
            records: vec![],
            handoffs: vec![],
            tickets: Default::default(),
            id,
            name: name.trim().into(),
            kind,
            stage: 0,
            notes: vec![String::new(); template.stages().len()],
            template,
            profiles: BTreeMap::new(),
            targets: BTreeMap::new(),
            attempts: vec![],
            current_attempt: BTreeMap::new(),
            repositories: Vec::new(),
        };
        for (i, stage) in template.stages().iter().enumerate() {
            if let Some(profile) = self.preset(Profile::for_stage(stage).client) {
                work.profiles.insert(i, profile.clone());
            }
        }
        for path in &self.repositories {
            work.include(path);
        }
        self.work.push(work);
        Ok(())
    }
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
                let mut library: Self =
                    serde_json::from_slice(&data).context("项目库文件无法读取")?;
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
            cli_defaults: vec![],
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
