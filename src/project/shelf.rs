//! Put a delivered item away. What it taught can first be written to project memory, which
//! every later stage of every item reads; its worktree and merged branches can then be removed.
use super::{Project, Work};
use crate::{
    memory::{Key, Memory, TEXT_LIMIT},
    workspace::{Workspace, git},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shelved {
    pub at: String,
    /// The project memory note written when the item was archived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<i64>,
}

impl Work {
    pub fn delivered(&self) -> bool {
        self.stage >= self.template.stages().len()
    }

    /// The item's own worktrees (one per repository; a pinned restart may add `<id>-<run>`).
    pub fn worktrees(&self) -> Vec<PathBuf> {
        let mut found = Vec::new();
        for repo in &self.repositories {
            let Ok(ws) = Workspace::open(repo) else {
                continue;
            };
            let Ok(entries) = fs::read_dir(ws.data.join("workflow-worktrees")) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if (name == self.id || name.starts_with(&format!("{}-", self.id)))
                    && entry.path().is_dir()
                {
                    found.push(entry.path());
                }
            }
        }
        found.sort();
        found
    }

    /// A memory note drafted from each stage's latest handoff, within the note size limit.
    pub fn memory_draft(&self, home: &Path, project: &str) -> Result<String> {
        let stages = self.template.stages();
        let notes = Memory::browse(&home.join("workflows").join(project))?;
        let mut by_run: HashMap<&str, &str> = HashMap::new();
        for note in &notes {
            if note["kind"] == "handoff"
                && note["successor"].is_null()
                && let (Some(run), Some(body)) = (note["run_id"].as_str(), note["body"].as_str())
            {
                by_run.entry(run).or_insert(body);
            }
        }
        let mut lines = Vec::new();
        for (i, stage) in stages.iter().enumerate() {
            if let Some(reason) = self.skipped.get(&i) {
                lines.push(format!("- {}：已跳过（{reason}）", stage.name));
                continue;
            }
            let body = self
                .attempts
                .iter()
                .rev()
                .filter(|a| a.stage == i)
                .find_map(|a| by_run.get(a.id.as_str()));
            let Some(body) = body else { continue };
            // Handoffs start with "<item> · <stage>"; keep only what was decided.
            let text = body
                .split_once('\n')
                .filter(|(head, _)| head.contains(" · "))
                .map_or(*body, |(_, rest)| rest)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if !text.is_empty() {
                lines.push(format!("- {}：{text}", stage.name));
            }
        }
        let head = format!("「{}」已交付。", self.name);
        let room = TEXT_LIMIT.saturating_sub(head.chars().count() + 8);
        let each = room / lines.len().max(1);
        let mut out = head;
        for line in lines {
            out.push('\n');
            if line.chars().count() > each {
                out.extend(line.chars().take(each.saturating_sub(1)));
                out.push('…');
            } else {
                out.push_str(&line);
            }
        }
        Ok(out)
    }
}

impl Project {
    /// Archive a delivered item, optionally writing `memory` as a project decision first.
    pub fn archive_work(&mut self, home: &Path, wid: &str, memory: &str) -> Result<Option<i64>> {
        let pid = self.id.clone();
        let work = self.work_by_id_mut(wid)?;
        ensure!(work.delivered(), "只有已交付的事项可以归档");
        ensure!(work.shelved.is_none(), "事项已归档");
        let memory = memory.trim();
        let id = if memory.is_empty() {
            None
        } else {
            ensure!(
                memory.chars().count() <= TEXT_LIMIT,
                "记忆最多 {TEXT_LIMIT} 字，当前 {} 字，请精简",
                memory.chars().count()
            );
            let mut db = Memory::open(&home.join("workflows").join(&pid))?;
            Some(db.add_manual(&Key::new("", "", ""), "decision", memory, None, None)?)
        };
        work.shelved = Some(Shelved {
            at: chrono::Utc::now().to_rfc3339(),
            memory: id,
        });
        Ok(id)
    }

    /// Delete an item that never ran: no attempts, no deliveries, no worktree. Anything an
    /// agent has touched is archived instead, so its record survives.
    pub fn remove_work(&mut self, home: &Path, wid: &str) -> Result<()> {
        let work = self.work_by_id_mut(wid)?;
        ensure!(
            work.attempts.is_empty() && work.deliveries.is_empty() && work.worktrees().is_empty(),
            "这个事项已经运行过，不能删除；交付后可以归档"
        );
        self.work.retain(|w| w.id != wid);
        if self.selected.as_deref() == Some(wid) {
            self.selected = self.work.last().map(|w| w.id.clone());
        }
        let dir = home.join("workflows").join(&self.id).join(wid);
        if dir.is_dir() {
            fs::remove_dir_all(dir)?;
        }
        Ok(())
    }
    pub fn unarchive_work(&mut self, wid: &str) -> Result<()> {
        let work = self.work_by_id_mut(wid)?;
        ensure!(work.shelved.take().is_some(), "事项未归档");
        Ok(())
    }
}

/// Remove an item's worktree. Git refuses when it holds uncommitted changes; the branch
/// `aep-stage/<id>` is kept, so nothing committed is lost.
pub fn remove_worktree(path: &Path) -> Result<()> {
    let common = git(
        path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let root = Path::new(common.trim())
        .parent()
        .context("找不到工作区所属仓库")?
        .to_path_buf();
    let dirty = git(path, &["status", "--porcelain"])?;
    ensure!(
        dirty.trim().is_empty(),
        "工作区 {} 还有未提交的改动，请先处理",
        path.display()
    );
    git(
        &root,
        &[
            "worktree",
            "remove",
            path.to_str().context("工作区路径无效")?,
        ],
    )?;
    Ok(())
}

/// One of an item's local branches (`aep-stage/<id>`, or `aep-stage/<id>-<run>` for a pinned
/// restart). `merged` means another local, non-item branch already contains its tip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ItemBranch {
    pub repository: PathBuf,
    pub name: String,
    pub merged: bool,
}

/// What a cleanup removed, and the unmerged branches it kept.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Cleaned {
    pub worktrees: usize,
    pub deleted: Vec<String>,
    pub kept: Vec<String>,
}

impl Work {
    pub fn branches(&self) -> Vec<ItemBranch> {
        let mut found = Vec::new();
        for repo in &self.repositories {
            let Ok(ws) = Workspace::open(repo) else {
                continue;
            };
            let exact = format!("refs/heads/aep-stage/{}", self.id);
            let pinned = format!("refs/heads/aep-stage/{}-*", self.id);
            let Ok(names) = git(
                &ws.root,
                &["for-each-ref", "--format=%(refname:short)", &exact, &pinned],
            ) else {
                continue;
            };
            for name in names.lines().map(str::trim).filter(|n| !n.is_empty()) {
                let merged = git(
                    &ws.root,
                    &[
                        "for-each-ref",
                        "--format=%(refname:short)",
                        "--contains",
                        name,
                        "refs/heads",
                    ],
                )
                .is_ok_and(|c| c.lines().any(|b| !b.trim().starts_with("aep-stage/")));
                found.push(ItemBranch {
                    repository: ws.root.clone(),
                    name: name.to_owned(),
                    merged,
                });
            }
        }
        found
    }

    /// Remove the item's worktrees (refused while one has uncommitted changes), then its
    /// branches: merged ones always, unmerged ones only when `force` (their commits stay
    /// reachable from the pinned stage refs `refs/aep/stages/*`).
    pub fn clean(&self, force: bool) -> Result<Cleaned> {
        let trees = self.worktrees();
        for path in &trees {
            remove_worktree(path)?;
        }
        let mut out = Cleaned {
            worktrees: trees.len(),
            ..Cleaned::default()
        };
        for b in self.branches() {
            if b.merged || force {
                git(&b.repository, &["branch", "-D", &b.name])?;
                out.deleted.push(b.name);
            } else {
                out.kept.push(b.name);
            }
        }
        Ok(out)
    }
}
