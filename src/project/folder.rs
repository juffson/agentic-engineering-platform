//! Discovering repositories under a plain workspace folder (specs/multi-repo-folder-project):
//! the folder itself is not a repository, it only groups repositories and keeps shared notes
//! (AGENTS.md / CLAUDE.md) at its top level.
use crate::workspace::Workspace;
use anyhow::{Result, ensure};
use serde::Serialize;
use std::{
    collections::{HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
};

pub const DEFAULT_DEPTH: u8 = 2;
pub const MAX_DEPTH: u8 = 4;
pub const MAX_DIRS: usize = 5000;
pub const NOTES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];

/// A git repository discovered below a workspace folder.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Found {
    pub path: PathBuf,
    pub relative: String,
    pub name: String,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Resolved {
    /// The path is (inside) a git repository: today's behaviour.
    Repository { root: PathBuf },
    /// Not a repository, but repositories were found below it.
    Folder {
        root: PathBuf,
        depth: u8,
        repositories: Vec<Found>,
        truncated: bool,
        notes: Vec<String>,
    },
    /// Not a repository and none below it (within `depth`).
    Empty { root: PathBuf, depth: u8, truncated: bool },
    Missing { path: PathBuf },
    NotDir { path: PathBuf },
}

/// Classify `path`: a repository (or inside one), a folder of repositories, an empty folder,
/// or a path that cannot be used. `depth` is how many levels of subdirectories to look into.
pub fn resolve(path: &Path, depth: u8) -> Result<Resolved> {
    let path = PathBuf::from(path.to_string_lossy().trim());
    if !path.exists() {
        return Ok(Resolved::Missing { path });
    }
    if !path.is_dir() {
        return Ok(Resolved::NotDir { path });
    }
    ensure!((1..=MAX_DEPTH).contains(&depth), "--depth 只能是 1 到 4");
    if let Ok(ws) = Workspace::open(&path) {
        return Ok(Resolved::Repository {
            root: ws.root.canonicalize()?,
        });
    }
    let root = path.canonicalize()?;
    let (repositories, truncated) = scan(&root, depth, MAX_DIRS);
    if repositories.is_empty() {
        return Ok(Resolved::Empty { root, depth, truncated });
    }
    let notes = notes(&root)
        .iter()
        .map(|path| path.file_name().unwrap_or_default().to_string_lossy().into_owned())
        .collect();
    Ok(Resolved::Folder { root, depth, repositories, truncated, notes })
}

/// Existing top-level AGENTS.md / CLAUDE.md of a workspace folder, in `NOTES` order.
pub fn notes(root: &Path) -> Vec<PathBuf> {
    NOTES
        .iter()
        .map(|name| root.join(name))
        .filter(|path| path.is_file())
        .collect()
}

/// The fixed texts of interaction §2.1 / spec R3; empty for `Repository`.
pub fn message(r: &Resolved) -> String {
    match r {
        Resolved::Repository { .. } => String::new(),
        Resolved::Missing { path } => format!("路径不存在：{}", path.display()),
        Resolved::NotDir { path } => format!("不是文件夹：{}", path.display()),
        Resolved::Empty { depth, truncated, .. } => {
            let mut text = format!("这个文件夹和它的子目录里没有 git 仓库（已查找 {depth} 层）");
            if *truncated {
                text.push_str("；文件夹太多，结果可能不完整");
            }
            text
        }
        Resolved::Folder { root, repositories, depth, .. } => format!(
            "{} 不是 git 仓库，在它下面找到 {} 个仓库（查找 {depth} 层）",
            root.display(),
            repositories.len()
        ),
    }
}

/// Friendly replacement for the git error when a path that should have been a repository
/// turns out to be a folder (or worse); never includes git's own output.
pub fn unreadable(path: &Path) -> anyhow::Error {
    match resolve(path, DEFAULT_DEPTH) {
        Ok(resolved) => {
            let text = message(&resolved);
            if matches!(resolved, Resolved::Folder { .. }) {
                anyhow::anyhow!("{text}；请在清单中选择要添加的仓库")
            } else {
                anyhow::anyhow!("{text}")
            }
        }
        Err(err) => anyhow::anyhow!("无法读取这个文件夹：{err}"),
    }
}

/// True when `dir` is a repository root git itself agrees on: `.git` exists (a directory, or
/// the file a worktree keeps — bare repositories have neither) and rev-parse resolves back
/// to `dir`, so a broken `.git` is skipped silently.
fn is_repository(dir: &Path) -> bool {
    dir.join(".git").exists()
        && Workspace::open(dir).is_ok_and(|ws| ws.root.canonicalize().is_ok_and(|root| root == dir))
}

/// Breadth-first walk below `root` (never a repository itself); `max` caps the directories
/// visited so tests can exercise truncation without waiting on MAX_DIRS.
fn scan(root: &Path, depth: u8, max: usize) -> (Vec<Found>, bool) {
    let mut queue = VecDeque::new();
    let mut visited = HashSet::from([root.to_path_buf()]);
    queue.push_back((root.to_path_buf(), 0u8, String::new()));
    let mut found = Vec::new();
    let mut truncated = false;
    let mut visited_count = 0usize;
    while let Some((dir, level, relative)) = queue.pop_front() {
        visited_count += 1;
        if visited_count > max {
            truncated = true;
            break;
        }
        // 读不了的目录（无权限等）直接跳过，不中断扫描（E7）。
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            if matches!(name.as_str(), "node_modules" | "target") || name.starts_with('.') {
                continue;
            }
            let Ok(metadata) = fs::metadata(entry.path()) else { continue };
            if !metadata.is_dir() {
                continue;
            }
            let Ok(real) = fs::canonicalize(entry.path()) else { continue };
            // 规范化后见过的路径不再处理：链接循环和重复链接都在这里结束（E5）。
            if !visited.insert(real.clone()) {
                continue;
            }
            let entry_relative = if relative.is_empty() {
                name.clone()
            } else {
                format!("{relative}/{name}")
            };
            if is_repository(&real) {
                // 命中仓库后不再进入它的内部，嵌套仓库由该仓库自己管理（R2、E3）。
                let name = real
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or(name);
                found.push(Found { path: real, relative: entry_relative, name });
            } else if level + 1 < depth {
                queue.push_back((real, level + 1, entry_relative));
            }
        }
    }
    found.sort_by(|a, b| a.relative.cmp(&b.relative));
    found.dedup_by(|a, b| a.path == b.path);
    (found, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::git;
    use std::os::unix::fs::PermissionsExt;

    fn repo(path: &Path) -> Result<()> {
        fs::create_dir_all(path)?;
        git(path, &["init"])?;
        git(path, &["config", "user.email", "a@example.com"])?;
        git(path, &["config", "user.name", "a"])?;
        git(path, &["commit", "--allow-empty", "-m", "init"])?;
        Ok(())
    }

    fn listed(resolved: &Resolved) -> Vec<(String, String)> {
        let Resolved::Folder { repositories, .. } = resolved else {
            panic!("expected a folder of repositories, got {resolved:?}")
        };
        repositories
            .iter()
            .map(|f| (f.relative.clone(), f.name.clone()))
            .collect()
    }

    #[test]
    fn repositories_are_listed_up_to_the_requested_depth() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        repo(&root.join("a"))?;
        repo(&root.join("group/b"))?;
        repo(&root.join("group/deep/c"))?;
        assert_eq!(
            listed(&resolve(root, 2)?),
            [("a".into(), "a".into()), ("group/b".into(), "b".into())]
        );
        assert_eq!(
            listed(&resolve(root, 3)?),
            [
                ("a".into(), "a".into()),
                ("group/b".into(), "b".into()),
                ("group/deep/c".into(), "c".into())
            ]
        );
        let Resolved::Folder { repositories, .. } = resolve(root, 3)? else {
            unreachable!()
        };
        let absolute = root.join("group/b").canonicalize()?;
        assert!(repositories.iter().any(|f| f.path == absolute));
        Ok(())
    }

    #[test]
    fn skipped_directories_hide_their_repositories() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        repo(&root.join("node_modules/x"))?;
        repo(&root.join("target/x"))?;
        repo(&root.join(".hidden/x"))?;
        let resolved = resolve(root, DEFAULT_DEPTH)?;
        assert_eq!(message(&resolved), "这个文件夹和它的子目录里没有 git 仓库（已查找 2 层）");
        Ok(())
    }

    #[test]
    fn repositories_inside_a_repository_are_not_listed() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        repo(&root.join("outer"))?;
        repo(&root.join("outer/inner"))?;
        assert_eq!(listed(&resolve(root, 3)?), [("outer".into(), "outer".into())]);
        Ok(())
    }

    #[test]
    fn worktrees_are_listed_and_bare_repositories_are_not() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        repo(&root.join("main"))?;
        git(&root.join("main"), &["worktree", "add", root.join("wt").to_str().unwrap()])?;
        git(root, &["init", "--bare", "bare.git"])?;
        assert_eq!(
            listed(&resolve(root, DEFAULT_DEPTH)?),
            [("main".into(), "main".into()), ("wt".into(), "wt".into())]
        );
        Ok(())
    }

    #[test]
    fn two_links_to_one_repository_list_it_once() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        repo(&root.join("real"))?;
        std::os::unix::fs::symlink(root.join("real"), root.join("link-a"))?;
        std::os::unix::fs::symlink(root.join("real"), root.join("link-b"))?;
        let Resolved::Folder { repositories, .. } = resolve(root, 1)? else {
            unreachable!()
        };
        assert_eq!(repositories.len(), 1);
        assert_eq!(repositories[0].path, root.join("real").canonicalize()?);
        assert_eq!(repositories[0].relative, "link-a");
        Ok(())
    }

    #[test]
    fn symlink_cycles_terminate() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        fs::create_dir_all(root.join("a"))?;
        std::os::unix::fs::symlink(root, root.join("a/up"))?;
        std::os::unix::fs::symlink(root.join("a"), root.join("a/self"))?;
        let resolved = resolve(root, MAX_DEPTH)?;
        assert_eq!(message(&resolved), "这个文件夹和它的子目录里没有 git 仓库（已查找 4 层）");
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn unreadable_subdirectories_are_skipped() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        repo(&root.join("ok"))?;
        repo(&root.join("locked/inner"))?;
        fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o000))?;
        let resolved = resolve(root, DEFAULT_DEPTH)?;
        fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o755))?;
        assert_eq!(listed(&resolved), [("ok".into(), "ok".into())]);
        Ok(())
    }

    #[test]
    fn the_directory_cap_truncates_the_scan() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        repo(&root.join("a1"))?;
        repo(&root.join("a2/late"))?;
        fs::create_dir_all(root.join("a3"))?;
        // 访问上限 1：只扫描了 root 本身，a2 下的仓库不再列出（a1 在发现时即命中）。
        let (found, truncated) = scan(&root, 2, 1);
        assert!(truncated);
        let listed: Vec<&str> = found.iter().map(|f| f.relative.as_str()).collect();
        assert_eq!(listed, ["a1"]);
        let (all, truncated) = scan(&root, 2, MAX_DIRS);
        assert!(!truncated);
        let listed: Vec<&str> = all.iter().map(|f| f.relative.as_str()).collect();
        assert_eq!(listed, ["a1", "a2/late"]);
        Ok(())
    }

    #[test]
    fn repository_paths_resolve_to_the_repository() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        repo(&root.join("r"))?;
        fs::create_dir_all(root.join("r/sub"))?;
        let expected = root.join("r").canonicalize()?;
        for path in [root.join("r"), root.join("r/sub")] {
            let Resolved::Repository { root } = resolve(&path, DEFAULT_DEPTH)? else {
                anyhow::bail!("expected a repository for {}", path.display())
            };
            assert_eq!(root, expected);
        }
        Ok(())
    }

    #[test]
    fn missing_and_non_directory_paths() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let missing = resolve(&temp.path().join("nope"), DEFAULT_DEPTH)?;
        assert_eq!(
            message(&missing),
            format!("路径不存在：{}", temp.path().join("nope").display())
        );
        let file = temp.path().join("file.txt");
        fs::write(&file, b"x")?;
        let not_dir = resolve(&file, DEFAULT_DEPTH)?;
        assert_eq!(message(&not_dir), format!("不是文件夹：{}", file.display()));
        Ok(())
    }

    #[test]
    fn notes_follow_the_fixed_order_and_existence() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        assert!(notes(root).is_empty());
        fs::write(root.join("CLAUDE.md"), b"")?;
        assert_eq!(notes(root), [root.join("CLAUDE.md")]);
        fs::write(root.join("AGENTS.md"), b"")?;
        assert_eq!(notes(root), [root.join("AGENTS.md"), root.join("CLAUDE.md")]);
        Ok(())
    }

    #[test]
    fn folder_notes_carry_the_file_names() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        repo(&root.join("a"))?;
        fs::write(root.join("AGENTS.md"), b"")?;
        let Resolved::Folder { notes, .. } = resolve(root, DEFAULT_DEPTH)? else {
            unreachable!()
        };
        assert_eq!(notes, ["AGENTS.md"]);
        Ok(())
    }

    #[test]
    fn message_texts() -> Result<()> {
        let root = PathBuf::from("/definitely/not/here");
        assert_eq!(
            message(&Resolved::Missing { path: root.clone() }),
            "路径不存在：/definitely/not/here"
        );
        assert_eq!(
            message(&Resolved::Empty { root: root.clone(), depth: 3, truncated: true }),
            "这个文件夹和它的子目录里没有 git 仓库（已查找 3 层）；文件夹太多，结果可能不完整"
        );
        assert_eq!(message(&Resolved::Repository { root: root.clone() }), "");
        let temp = tempfile::tempdir()?;
        repo(&temp.path().join("a"))?;
        repo(&temp.path().join("b"))?;
        assert_eq!(
            message(&resolve(temp.path(), DEFAULT_DEPTH)?),
            format!(
                "{} 不是 git 仓库，在它下面找到 2 个仓库（查找 2 层）",
                temp.path().canonicalize()?.display()
            )
        );
        Ok(())
    }

    #[test]
    fn depth_out_of_range_is_an_error() -> Result<()> {
        let temp = tempfile::tempdir()?;
        assert!(resolve(temp.path(), 0).is_err());
        assert!(resolve(temp.path(), MAX_DEPTH + 1).is_err());
        Ok(())
    }

    #[test]
    fn unreadable_points_multiple_repositories_at_the_list() -> Result<()> {
        let temp = tempfile::tempdir()?;
        repo(&temp.path().join("a"))?;
        assert_eq!(
            unreadable(temp.path()).to_string(),
            format!(
                "{} 不是 git 仓库，在它下面找到 1 个仓库（查找 2 层）；请在清单中选择要添加的仓库",
                temp.path().canonicalize()?.display()
            )
        );
        assert_eq!(
            unreadable(&temp.path().join("nope")).to_string(),
            format!("路径不存在：{}", temp.path().join("nope").display())
        );
        Ok(())
    }
}
