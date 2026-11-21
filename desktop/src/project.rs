use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};
use uuid::Uuid;

pub const STAGES: [(&str, &str); 5] = [
    ("需求定义", "记录用户目标、范围和验收标准"),
    ("方案设计", "记录技术方案、涉及仓库和接口约定"),
    ("开发实现", "跟进各仓库的实现任务和代码变更"),
    ("测试审查", "核对验收结果、审查意见和遗留问题"),
    ("发布归档", "记录发布版本、交付范围和证据位置"),
];

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
    pub notes: [String; STAGES.len()],
    pub repositories: Vec<PathBuf>,
}
impl Work {
    pub fn stage_name(&self) -> &'static str {
        STAGES.get(self.stage).map_or("已交付", |s| s.0)
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
    pub fn add_work(&mut self, name: &str, kind: Kind) -> Result<()> {
        ensure!(!name.trim().is_empty(), "请填写事项名称");
        let id = Uuid::new_v4().simple().to_string();
        self.selected = Some(id.clone());
        let mut work = Work {
            tickets: Default::default(),
            id,
            name: name.trim().into(),
            kind,
            stage: 0,
            notes: Default::default(),
            repositories: Vec::new(),
        };
        for path in &self.repositories {
            work.include(path);
        }
        self.work.push(work);
        Ok(())
    }
}
#[derive(Default, Clone, Serialize, Deserialize)]
pub struct Library {
    pub(crate) revision: u64,
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
        });
        self.active = Some(id);
        Ok(())
    }
    pub fn change(&mut self, dir: &Path, edit: impl FnOnce(&mut Self) -> Result<()>) -> Result<()> {
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
        edit(&mut next)?;
        next.revision += 1;
        crate::lifecycle::save(dir, "projects.json", &next)?;
        *self = next;
        lock.unlock()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
