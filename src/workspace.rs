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
