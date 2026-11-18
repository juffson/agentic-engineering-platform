use crate::{Scope, memory::Key};
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    process::Command,
};

pub fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .context("start git")?;
    if !out.status.success() {
        bail!(
            "git {}: {}",
            args[0],
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8(out.stdout)?)
}
pub fn commit(repo: &Path, rev: &str) -> Result<String> {
    Ok(git(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{rev}^{{commit}}"),
        ],
    )?
    .trim_end()
    .into())
}
pub fn ident(id: &str, max: usize) -> Result<()> {
    ensure!(
        !id.is_empty()
            && id.len() <= max
            && !id.starts_with('-')
            && id
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-'),
        "invalid identifier: {id}"
    );
    Ok(())
}
#[derive(Deserialize)]
pub struct Task {
    pub title: Option<String>,
    pub allowed_paths: Vec<String>,
    pub check: Vec<String>,
}
pub struct Repo {
    pub root: PathBuf,
    pub data: PathBuf,
}
impl Repo {
    pub fn open(dir: &Path) -> Result<Self> {
        let root = PathBuf::from(git(dir, &["rev-parse", "--show-toplevel"])?.trim_end());
        let data = root
            .join(git(&root, &["rev-parse", "--git-common-dir"])?.trim_end())
            .canonicalize()?
            .join("aep");
        Ok(Self { root, data })
    }
    pub fn scope(&self, input: &Scope) -> Result<Key> {
        let task = input.task.as_deref().unwrap_or("");
        if !task.is_empty() {
            ident(task, 63)?;
        }
        let spec = input
            .spec
            .as_ref()
            .map(|r| commit(&self.root, r))
            .transpose()?
            .unwrap_or_default();
        let candidate = input
            .candidate
            .as_ref()
            .map(|r| commit(&self.root, r))
            .transpose()?
            .unwrap_or_default();
        if !candidate.is_empty() {
            git(
                &self.root,
                &["merge-base", "--is-ancestor", &spec, &candidate],
            )?;
        }
        Ok(Key::new(task, &spec, &candidate))
    }
    pub fn task(&self, sha: &str, name: &str) -> Result<(String, Task)> {
        ident(name, 63)?;
        let path = format!("{sha}:specs/{name}");
        let spec = git(&self.root, &["show", &format!("{path}/spec.md")])?;
        let task: Task =
            serde_json::from_str(&git(&self.root, &["show", &format!("{path}/task.json")])?)?;
        ensure!(
            !task.allowed_paths.is_empty()
                && task
                    .allowed_paths
                    .iter()
                    .all(|s| s.split('/').all(|p| !matches!(p, "" | "." | ".." | ".git"))),
            "invalid allowed_paths"
        );
        ensure!(
            !task.check.is_empty() && task.check.iter().all(|s| !s.is_empty()),
            "check must contain a command"
        );
        Ok((spec, task))
    }
    fn remote_tip(&self, remote: &str, r: &str) -> Result<Option<String>> {
        ensure!(!remote.starts_with('-'), "invalid remote");
        Ok(git(&self.root, &["ls-remote", "--refs", remote, r])?
            .split_whitespace()
            .next()
            .map(str::to_owned))
    }
    pub fn dispatch(&self, name: &str, rev: &str, remote: &str) -> Result<()> {
        let sha = commit(&self.root, rev)?;
        self.task(&sha, name)?;
        let r = format!("refs/heads/aep-task/{name}");
        if let Some(prev) = self.remote_tip(remote, &r)? {
            ensure!(prev == sha, "task already dispatched at another revision");
        } else if let Err(e) = git(
            &self.root,
            &[
                "push",
                &format!("--force-with-lease={r}:"),
                remote,
                &format!("{sha}:{r}"),
            ],
        ) && self.remote_tip(remote, &r)?.as_deref() != Some(&sha)
        {
            return Err(e);
        }
        println!("{r} {sha}");
        Ok(())
    }
    pub fn dispatched(&self, name: &str, remote: &str) -> Result<String> {
        ident(name, 63)?;
        let r = format!("refs/heads/aep-task/{name}");
        let sha = self
            .remote_tip(remote, &r)?
            .context("no dispatch found for task")?;
        git(&self.root, &["fetch", "--no-tags", remote, &r])?;
        commit(&self.root, &sha)
    }
    pub fn changes(&self, base: &str, head: &str, allowed: &[String]) -> Result<Vec<String>> {
        git(&self.root, &["merge-base", "--is-ancestor", base, head])?;
        let diff = git(
            &self.root,
            &["diff", "--no-renames", "--name-only", "-z", base, head],
        )?;
        let paths: Vec<String> = diff.split_terminator('\0').map(str::to_owned).collect();
        for path in &paths {
            ensure!(
                allowed.contains(path),
                "candidate touched files outside the task: {path}"
            );
        }
        Ok(paths)
    }
}
