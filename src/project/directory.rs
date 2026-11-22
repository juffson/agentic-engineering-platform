//! Resolve a Git directory to the same Studio catalog and work item as the CLI.
use super::{Library, Project};
use crate::workspace::{Workspace, git};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
pub struct Binding {
    pub home: PathBuf,
    pub project: String,
    pub work: String,
}

impl Binding {
    pub fn file(cwd: &Path) -> Option<PathBuf> {
        let dir = git(cwd, &["rev-parse", "--absolute-git-dir"]).ok()?;
        Some(Path::new(dir.trim_end()).join("aep/project-binding.json"))
    }

    pub fn read(path: &Path) -> Result<Option<Self>> {
        match fs::File::open(path) {
            Ok(file) => Ok(Some(
                serde_json::from_reader(file).context("无法读取项目绑定")?,
            )),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DirectoryAction {
    Open,
    Restore,
}

pub struct DirectoryProject {
    pub home: PathBuf,
    pub library: Library,
    pub bound_work: bool,
}

impl Project {
    pub fn includes_workspace(&self, ws: &Workspace) -> bool {
        self.repositories.contains(&ws.root)
            || self
                .repositories
                .iter()
                .any(|path| Workspace::open(path).is_ok_and(|other| other.data == ws.data))
    }
}

impl DirectoryProject {
    pub fn resolve(
        ws: &Workspace,
        default_home: &Path,
        explicit_home: Option<&Path>,
        action: DirectoryAction,
    ) -> Result<Self> {
        let binding = Binding::file(&ws.root)
            .map(|p| Binding::read(&p))
            .transpose()?
            .flatten();
        let home = explicit_home
            .or_else(|| binding.as_ref().map(|b| b.home.as_path()))
            .unwrap_or(default_home)
            .to_path_buf();
        let binding = binding.filter(|b| b.home == home);
        let mut library = Library::open(&home)?;
        // An ordinary relaunch keeps the user's last selection in this catalog.
        if action == DirectoryAction::Restore
            && let Some(p) = library.project()
        {
            let bound_work = binding
                .as_ref()
                .is_some_and(|b| b.project == p.id && p.selected().is_some_and(|w| w.id == b.work));
            return Ok(Self {
                home,
                library,
                bound_work,
            });
        }
        let bound_work = binding.is_some();
        if let Some(b) = binding {
            let project = library
                .projects
                .iter()
                .find(|p| p.id == b.project)
                .context("目录绑定的项目不存在，请检查项目库或使用 aep project bind 重新绑定")?;
            anyhow::ensure!(
                project.work.iter().any(|w| w.id == b.work),
                "目录绑定的开发事项不存在，请重新绑定"
            );
            if library.active.as_ref() != Some(&b.project)
                || project.selected.as_ref() != Some(&b.work)
            {
                library.change(&home, |lib| {
                    lib.active = Some(b.project);
                    lib.project_mut()?.selected = Some(b.work);
                    Ok(())
                })?;
            }
        } else if library.projects.is_empty() {
            let name = ws
                .root
                .file_name()
                .context("无法读取仓库名称")?
                .to_string_lossy();
            library.change(&home, |lib| lib.create(&name, vec![ws.root.clone()]))?;
        } else {
            let mut matches: Vec<&Project> = library
                .projects
                .iter()
                .filter(|p| p.repositories.contains(&ws.root))
                .collect();
            if matches.is_empty() {
                matches = library
                    .projects
                    .iter()
                    .filter(|p| p.includes_workspace(ws))
                    .collect();
            }
            if !matches
                .iter()
                .any(|p| library.active.as_ref() == Some(&p.id))
            {
                match matches.as_slice() {
                    [] => bail!(
                        "目录 {} 尚未关联项目，请在目标项目的「关联仓库」中添加此目录",
                        ws.root.display()
                    ),
                    [p] => {
                        let id = p.id.clone();
                        library.change(&home, |lib| {
                            lib.active = Some(id);
                            Ok(())
                        })?;
                    }
                    _ => bail!(
                        "目录关联多个项目（{}），请先在项目库选择一个，再打开目录",
                        matches
                            .iter()
                            .map(|p| p.name.as_str())
                            .collect::<Vec<_>>()
                            .join("、")
                    ),
                }
            }
        }
        Ok(Self {
            home,
            library,
            bound_work,
        })
    }
}
