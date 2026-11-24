//! Bring what Claude Code, Codex and ZCode already know about a repository into a project:
//! Claude's memory files become project memory, and existing worktree branches become items
//! that continue from 开发实现 on their own `aep-stage/<id>` branch.
use super::{Kind, Project};
use crate::{
    memory::{Key, Memory, TEXT_LIMIT},
    workflow::{Role, Template},
    workspace::{Workspace, git},
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Found {
    pub repository: PathBuf,
    pub memories: Vec<MemoryFile>,
    /// Instruction files every agent already reads from the repository itself.
    pub rules: Vec<String>,
    pub claude_sessions: usize,
    pub codex_sessions: usize,
    pub zcode_tasks: usize,
    pub worktrees: Vec<FoundWorktree>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MemoryFile {
    pub file: String,
    pub title: String,
    pub description: String,
    pub chars: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FoundWorktree {
    pub path: PathBuf,
    pub branch: Option<String>,
    /// Commits on the branch that the repository's checked-out branch does not have.
    pub ahead: usize,
    /// Uncommitted entries; these stay in the original worktree and are not imported.
    pub dirty: usize,
    /// "Claude", "ZCode" or "Git", from where the worktree lives.
    pub source: String,
}

fn home() -> Result<PathBuf> {
    std::env::home_dir().context("找不到用户目录")
}

/// Claude Code keeps per-folder state under `~/.claude/projects/<path with / and . as ->`.
fn claude_dir(repo: &Path) -> Result<PathBuf> {
    let slug: String = repo
        .to_string_lossy()
        .chars()
        .map(|c| if matches!(c, '/' | '.') { '-' } else { c })
        .collect();
    Ok(std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or(home()?.join(".claude"))
        .join("projects")
        .join(slug))
}

/// `name` and `description` from a memory file's front matter, and the text after it.
fn parse_memory(text: &str) -> (Option<String>, String, String) {
    let Some(rest) = text.strip_prefix("---\n") else {
        return (None, String::new(), text.trim().to_owned());
    };
    let Some((head, body)) = rest.split_once("\n---") else {
        return (None, String::new(), text.trim().to_owned());
    };
    let field = |key: &str| {
        head.lines()
            .find_map(|l| l.trim().strip_prefix(&format!("{key}:")))
            .map(|v| v.trim().trim_matches('"').to_owned())
    };
    (
        field("name"),
        field("description").unwrap_or_default(),
        body.trim_start_matches('-').trim().to_owned(),
    )
}

fn memory_files(repo: &Path) -> Result<Vec<(PathBuf, MemoryFile)>> {
    let dir = claude_dir(repo)?.join("memory");
    let Ok(entries) = fs::read_dir(&dir) else {
        return Ok(vec![]);
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let file = entry.file_name().to_string_lossy().into_owned();
        if !file.ends_with(".md") || file == "MEMORY.md" || !entry.path().is_file() {
            continue;
        }
        let text = fs::read_to_string(entry.path()).unwrap_or_default();
        let (name, description, body) = parse_memory(&text);
        out.push((
            entry.path(),
            MemoryFile {
                title: name.unwrap_or_else(|| file.trim_end_matches(".md").to_owned()),
                file,
                description,
                chars: body.chars().count(),
            },
        ));
    }
    out.sort_by(|a, b| a.1.file.cmp(&b.1.file));
    Ok(out)
}

fn inside(path: &str, repo: &Path) -> bool {
    let p = Path::new(path);
    p == repo || p.starts_with(repo)
}

/// Codex sessions whose recorded working directory is the repository (first line of each log).
fn codex_sessions(repo: &Path) -> Result<usize> {
    let root = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or(home()?.join(".codex"))
        .join("sessions");
    let mut count = 0;
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "jsonl")
                && let Ok(f) = fs::File::open(&path)
                && let Some(Ok(line)) = BufReader::new(f).lines().next()
                && let Ok(v) = serde_json::from_str::<serde_json::Value>(&line)
                && v["payload"]["cwd"]
                    .as_str()
                    .is_some_and(|c| inside(c, repo))
            {
                count += 1;
            }
        }
    }
    Ok(count)
}

fn zcode_tasks(repo: &Path) -> usize {
    let Ok(home) = home() else { return 0 };
    let db = home.join(".zcode/v2/tasks-index.sqlite");
    let Ok(conn) =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return 0;
    };
    let repo = repo.to_string_lossy();
    conn.query_row(
        "SELECT count(*) FROM tasks WHERE workspace_path = ?1 OR workspace_path LIKE ?1 || '/%'",
        [repo.as_ref()],
        |r| r.get::<_, i64>(0),
    )
    .map_or(0, |n| n as usize)
}

fn worktrees(root: &Path) -> Result<Vec<FoundWorktree>> {
    let ws = Workspace::open(root)?;
    let ours = ws.data.join("workflow-worktrees");
    let list = git(root, &["worktree", "list", "--porcelain"])?;
    let mut found = Vec::new();
    for block in list.split("\n\n") {
        let mut path = None;
        let mut branch = None;
        for line in block.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(PathBuf::from(p));
            } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
                branch = Some(b.to_owned());
            }
        }
        let Some(path) = path else { continue };
        if path == root || path.starts_with(&ours) || !path.is_dir() {
            continue;
        }
        found.push((path, branch));
    }
    // `git status` takes most of a second in a large checkout: look at all of them at once.
    Ok(std::thread::scope(|s| {
        let jobs: Vec<_> = found
            .into_iter()
            .map(|(path, branch)| {
                s.spawn(move || {
                    let ahead = branch
                        .as_deref()
                        .and_then(|b| git(root, &["rev-list", "--count", b, "^HEAD"]).ok())
                        .and_then(|n| n.trim().parse().ok())
                        .unwrap_or(0);
                    let dirty = git(&path, &["status", "--porcelain"])
                        .map(|s| s.lines().count())
                        .unwrap_or(0);
                    let p = path.to_string_lossy();
                    let source = if p.contains("/.zcode/worktrees/") {
                        "ZCode"
                    } else if p.contains("/.claude/worktrees/") {
                        "Claude"
                    } else {
                        "Git"
                    };
                    FoundWorktree {
                        source: source.into(),
                        path,
                        branch,
                        ahead,
                        dirty,
                    }
                })
            })
            .collect();
        jobs.into_iter().filter_map(|j| j.join().ok()).collect()
    }))
}

/// Everything importable for a repository folder.
pub fn scan(repo: &Path) -> Result<Found> {
    let root = Workspace::open(repo)?.root;
    let claude = claude_dir(&root)?;
    let claude_sessions = fs::read_dir(&claude)
        .map(|d| {
            d.flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
                .count()
        })
        .unwrap_or(0);
    Ok(Found {
        memories: memory_files(&root)?.into_iter().map(|(_, m)| m).collect(),
        rules: ["CLAUDE.md", "AGENTS.md"]
            .into_iter()
            .filter(|f| root.join(f).is_file())
            .map(str::to_owned)
            .collect(),
        claude_sessions,
        codex_sessions: codex_sessions(&root)?,
        zcode_tasks: zcode_tasks(&root),
        worktrees: worktrees(&root)?,
        repository: root,
    })
}

impl Project {
    /// Copy the chosen Claude memory files into project memory, one note each. A file imported
    /// before is skipped when unchanged and becomes a new version of its note when it changed.
    /// Returns the IDs written.
    pub fn import_memories(&self, home: &Path, repo: &Path, files: &[String]) -> Result<Vec<i64>> {
        let available = memory_files(repo)?;
        let dir = home.join("workflows").join(&self.id);
        // The newest version of each note, to update an earlier import of the same file.
        let current: Vec<(i64, String)> = Memory::browse(&dir)?
            .iter()
            .filter(|n| n["successor"].is_null())
            .filter_map(|n| Some((n["id"].as_i64()?, n["body"].as_str()?.to_owned())))
            .collect();
        let mut db = Memory::open(&dir)?;
        let mut ids = Vec::new();
        for file in files {
            let (path, meta) = available
                .iter()
                .find(|(_, m)| &m.file == file)
                .with_context(|| format!("找不到 Claude 记忆 {file}"))?;
            let (_, description, body) = parse_memory(&fs::read_to_string(path)?);
            let mut note = format!("【导入自 Claude 记忆：{}】", meta.title);
            if !description.is_empty() {
                note.push_str(&description);
            }
            note.push('\n');
            note.push_str(&body);
            if note.chars().count() > TEXT_LIMIT {
                let tail = format!("…（全文见 {}）", path.display());
                let keep = TEXT_LIMIT.saturating_sub(tail.chars().count());
                note = note.chars().take(keep).collect::<String>() + &tail;
            }
            let marker = format!("【导入自 Claude 记忆：{}】", meta.title);
            match current.iter().find(|(_, body)| body.starts_with(&marker)) {
                Some((_, body)) if body == &note => {}
                // The file changed since it was imported: a new version of that note.
                Some((id, _)) => ids.push(db.revise(*id, crate::memory::Kind::Decision, &note)?),
                None => ids.push(db.add_manual(&Key::new("", "", ""), "decision", &note, None, None)?),
            }
        }
        Ok(ids)
    }

    /// Turn an existing branch into an item that starts at the build stage. The item gets its
    /// own `aep-stage/<id>` branch at the same commit, so the original worktree is left alone.
    pub fn import_branch(&mut self, repo: &Path, branch: &str, from: &Path) -> Result<String> {
        let root = Workspace::open(repo)?.root.canonicalize()?;
        let tip = git(
            &root,
            &["rev-parse", "--verify", &format!("refs/heads/{branch}")],
        )
        .with_context(|| format!("分支 {branch} 不存在"))?
        .trim()
        .to_owned();
        ensure!(
            self.repositories.contains(&root),
            "仓库 {} 不在这个项目里",
            root.display()
        );
        let template = Template::Software;
        let build = template
            .stages()
            .iter()
            .position(|s| s.role == Role::Build)
            .context("模板没有开发阶段")?;
        self.add_work(branch, Kind::Feature, template)?;
        let work = self.selected_mut()?;
        let id = work.id.clone();
        for i in 0..build {
            let reason = format!("从已有分支 {branch} 导入");
            work.notes[i] = format!("已跳过：{reason}");
            work.skipped.insert(i, reason);
        }
        work.stage = build;
        work.notes[build] = format!(
            "接手已有分支 `{branch}`（原工作区 {}）上的工作：先读该分支的提交记录和相对主分支的改动，弄清目标和进度，补完未完成的部分，并让测试通过。",
            from.display()
        );
        git(&root, &["branch", &format!("aep-stage/{id}"), &tip])?;
        Ok(id)
    }
}
