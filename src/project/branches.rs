//! A repository's local branches, the worktrees they are checked out in, and the recent
//! commit graph that connects them, for the Studio branch page.
use crate::workspace::{Workspace, git};
use anyhow::Result;
use serde::Serialize;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Commit {
    pub sha: String,
    pub parents: Vec<String>,
    pub subject: String,
    pub author: String,
    /// Unix seconds.
    pub time: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Branch {
    pub name: String,
    pub sha: String,
    /// Commits the branch has that the repository's checked-out HEAD lacks, and the reverse.
    pub ahead: u64,
    pub behind: u64,
    /// Checked out in the repository itself.
    pub current: bool,
    pub worktree: Option<Worktree>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Worktree {
    pub path: PathBuf,
    /// "Claude", "ZCode", "AEP" or "Git", from where the worktree lives.
    pub source: String,
    /// Tracked files with uncommitted changes, and untracked files.
    pub changed: usize,
    pub untracked: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Graph {
    pub repository: PathBuf,
    pub head: Option<String>,
    pub branches: Vec<Branch>,
    /// Newest first, parents after children (topological order).
    pub commits: Vec<Commit>,
    /// More history exists beyond `commits`.
    pub truncated: bool,
}

pub fn source(path: &std::path::Path, branch: Option<&str>) -> &'static str {
    let p = path.to_string_lossy();
    if branch.is_some_and(|b| b.starts_with(STAGE_PREFIX)) {
        "AEP"
    } else if p.contains("/.zcode/worktrees/") {
        "ZCode"
    } else if p.contains("/.claude/worktrees/") {
        "Claude"
    } else {
        "Git"
    }
}

/// Branches of AEP stage worktrees: `aep-stage/<checkout>`.
pub const STAGE_PREFIX: &str = "aep-stage/";

/// Where new stage worktrees of a repository go: under the Studio catalog, outside `.git`
/// (Claude Code refuses to write anywhere inside `.git`).
pub fn worktree_home(catalog: &Path, root: &Path) -> PathBuf {
    catalog
        .join("worktrees")
        .join(root.to_string_lossy().replace(['/', '\\', ':'], "-"))
}

/// The stage worktrees of a repository, wherever they live: (checkout name, path).
pub fn stage_worktrees(root: &Path) -> Vec<(String, PathBuf)> {
    worktrees(root)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(path, branch)| {
            let checkout = branch?.strip_prefix(STAGE_PREFIX)?.to_owned();
            Some((checkout, path))
        })
        .collect()
}

/// `git status` counts for a checkout: changed tracked files, untracked files.
pub fn status(path: &std::path::Path) -> (usize, usize) {
    let Ok(out) = git(path, &["status", "--porcelain"]) else {
        return (0, 0);
    };
    // Agent tools keep their own state in the worktree (`.zcode/`, `.claude/`); not changes.
    let lines: Vec<&str> = out
        .lines()
        .filter(|l| !matches!(l.get(3..), Some(".zcode/" | ".claude/")))
        .collect();
    let untracked = lines.iter().filter(|l| l.starts_with("??")).count();
    (lines.len() - untracked, untracked)
}

/// Every worktree of the repository (including the main checkout) and its branch.
pub fn worktrees(root: &std::path::Path) -> Result<Vec<(PathBuf, Option<String>)>> {
    let list = git(root, &["worktree", "list", "--porcelain"])?;
    let mut out = Vec::new();
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
        if let Some(path) = path.filter(|p| p.is_dir()) {
            out.push((path, branch));
        }
    }
    Ok(out)
}

pub fn graph(repo: &std::path::Path, limit: usize) -> Result<Graph> {
    let root = Workspace::open(repo)?.root;
    let head = git(&root, &["branch", "--show-current"])
        .ok()
        .map(|b| b.trim().to_owned())
        .filter(|b| !b.is_empty());
    let trees = worktrees(&root)?;
    // `git status` is slow in a large checkout: count every worktree at once.
    let counts: HashMap<PathBuf, (usize, usize)> = std::thread::scope(|s| {
        let jobs: Vec<_> = trees
            .iter()
            .filter(|(p, _)| p != &root)
            .map(|(p, _)| s.spawn(move || (p.clone(), status(p))))
            .collect();
        jobs.into_iter().filter_map(|j| j.join().ok()).collect()
    });
    let by_branch: HashMap<&str, &PathBuf> = trees
        .iter()
        .filter(|(p, _)| p != &root)
        .filter_map(|(p, b)| Some((b.as_deref()?, p)))
        .collect();
    let refs = git(
        &root,
        &[
            "for-each-ref",
            "refs/heads",
            "--sort=-committerdate",
            "--format=%(refname:short)%1f%(objectname)%1f%(ahead-behind:HEAD)",
        ],
    )?;
    let mut branches = Vec::new();
    for line in refs.lines() {
        let mut parts = line.split('\x1f');
        let (Some(name), Some(sha)) = (parts.next(), parts.next()) else {
            continue;
        };
        let mut counts_ab = parts.next().unwrap_or("").split_whitespace();
        let ahead = counts_ab.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        let behind = counts_ab.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        let worktree = by_branch.get(name).map(|p| {
            let p: &PathBuf = p;
            let (changed, untracked) = counts.get(p).copied().unwrap_or_default();
            Worktree {
                path: p.clone(),
                source: source(p, Some(name)).into(),
                changed,
                untracked,
            }
        });
        branches.push(Branch {
            current: head.as_deref() == Some(name),
            name: name.to_owned(),
            sha: sha.to_owned(),
            ahead,
            behind,
            worktree,
        });
    }
    let log = git(
        &root,
        &[
            "log",
            "--branches",
            "HEAD",
            "--topo-order",
            &format!("--max-count={}", limit + 1),
            "--format=%H%x1f%P%x1f%s%x1f%an%x1f%ct",
        ],
    )?;
    let mut commits: Vec<Commit> = log
        .lines()
        .filter_map(|line| {
            let mut p = line.split('\x1f');
            Some(Commit {
                sha: p.next()?.to_owned(),
                parents: p.next()?.split_whitespace().map(str::to_owned).collect(),
                subject: p.next()?.to_owned(),
                author: p.next()?.to_owned(),
                time: p.next()?.parse().ok()?,
            })
        })
        .collect();
    let truncated = commits.len() > limit;
    commits.truncate(limit);
    Ok(Graph {
        repository: root,
        head,
        branches,
        commits,
        truncated,
    })
}
