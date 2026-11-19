use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    fs::{self, File, TryLockError},
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

pub fn validate_task_target(repo: &Path, target: &str, spec: &str, task: &str) -> Result<()> {
    ident(task, 63)?;
    let remedy = format!(
        "create a new task version from selected target {target}, commit its requirements under \
         specs/<new-task>/, and dispatch it with \
         `aep dispatch --task <new-task> --rev <new-spec-sha>`"
    );
    git(repo, &["merge-base", "--is-ancestor", target, spec]).with_context(|| {
        format!(
            "cannot use task {task} at pinned Spec {spec}: selected target {target} must be an \
             ancestor of the Spec; {remedy}"
        )
    })?;
    let diff = git(
        repo,
        &["diff", "--no-renames", "--name-only", "-z", target, spec],
    )?;
    let prefix = format!("specs/{task}/");
    for path in diff.split_terminator('\0') {
        ensure!(
            path.starts_with(&prefix),
            "pinned Spec {spec} changes {path:?} outside {prefix} relative to selected target \
             {target}; {remedy}"
        );
    }
    Ok(())
}

pub fn read(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(
        &fs::read(path).with_context(|| format!("read {}", path.display()))?,
    )?)
}

#[derive(Clone, Debug)]
pub struct Workspace {
    pub root: PathBuf,
    pub data: PathBuf,
}
impl Workspace {
    pub fn open(dir: &Path) -> Result<Self> {
        let root = PathBuf::from(git(dir, &["rev-parse", "--show-toplevel"])?.trim_end());
        let data = root
            .join(git(&root, &["rev-parse", "--git-common-dir"])?.trim_end())
            .canonicalize()?
            .join("aep");
        Ok(Self { root, data })
    }
}

pub fn status(data: &Path, id: &str) -> Result<Value> {
    ident(id, 64)?;
    let folder = data.join("runs").join(id);
    ensure!(folder.is_dir(), "run not found: {id}");
    let lock = folder.join("owner.lock");
    let path = folder.join("evidence.json");
    if !lock.exists() {
        return if path.exists() {
            read(&path)
        } else {
            Ok(json!({"run_id":id,"status":"initializing"}))
        };
    }
    let file = File::open(lock)?;
    let idle = match file.try_lock_shared() {
        Ok(()) => true,
        Err(TryLockError::WouldBlock) => false,
        Err(TryLockError::Error(e)) => return Err(e.into()),
    };
    if !path.exists() {
        let state = if idle { "unstarted" } else { "initializing" };
        return Ok(json!({"run_id": id, "status": state}));
    }
    let mut record = read(&path)?;
    if idle && record["status"] == "running" && record.get("request").is_some() {
        record["status"] = json!("needs_reconciliation");
    }
    Ok(record)
}
