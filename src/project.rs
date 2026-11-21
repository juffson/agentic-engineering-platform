use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};
use uuid::Uuid;

use crate::workflow::{Client, Profile, Template};
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
#[derive(Clone, Serialize, Deserialize)]
pub struct Work {
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
    pub id: String,
    pub name: String,
    pub repositories: Vec<PathBuf>,
    pub work: Vec<Work>,
    pub selected: Option<String>,
    #[serde(default)]
    pub cli_defaults: Vec<Profile>,
}
impl Project {
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
    pub fn preset(&self, client: Client) -> Option<&Profile> {
        self.cli_defaults.iter().find(|p| p.client == client)
    }
    pub fn add_work(&mut self, name: &str, kind: Kind, template: Template) -> Result<()> {
        ensure!(!name.trim().is_empty(), "请填写事项名称");
        let id = Uuid::new_v4().simple().to_string();
        self.selected = Some(id.clone());
        let mut work = Work {
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
            Ok(data) => Ok(serde_json::from_slice(&data).context("项目库文件无法读取")?),
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
