//! Local read model used by the native studio. No execution or approval operations.
use crate::{
    memory::Memory,
    workspace::{Workspace, ident, status},
};
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, FixedOffset};
use serde_json::Value;
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::Path,
};

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub runs: Vec<Run>,
    pub notes: Vec<Value>,
    pub issues: Vec<String>,
}
#[derive(Clone, Debug)]
pub struct Run {
    pub id: String,
    pub record: Value,
}
impl Run {
    pub fn text(&self, key: &str) -> &str {
        self.record[key].as_str().unwrap_or("")
    }
    fn timestamp(&self) -> Option<DateTime<FixedOffset>> {
        DateTime::parse_from_rfc3339(self.text("started_at")).ok()
    }
}
impl Snapshot {
    pub fn load(ws: &Workspace) -> Self {
        let mut snapshot = Self::default();
        if let Err(error) = snapshot.load_runs(ws) {
            snapshot.issues.push(format!("运行记录：{error:#}"));
        }
        match Memory::browse(&ws.data) {
            Ok(notes) => snapshot.notes = notes,
            Err(error) => snapshot.issues.push(format!("共享记忆：{error:#}")),
        }
        snapshot
    }
    fn load_runs(&mut self, ws: &Workspace) -> Result<()> {
        let runs = ws.data.join("runs");
        if !runs.try_exists()? {
            return Ok(());
        }
        for entry in fs::read_dir(runs)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let Some(id) = name.to_str() else { continue };
            if ident(id, 64).is_err() {
                continue;
            }
            match status(&ws.data, id) {
                Ok(record) if record.is_object() => self.runs.push(Run {
                    id: id.to_owned(),
                    record,
                }),
                Ok(_) => self.issues.push(format!("{id}：无效的运行记录")),
                Err(error) => self.issues.push(format!("{id}：{error:#}")),
            }
        }
        self.runs
            .sort_by_cached_key(|run| (std::cmp::Reverse(run.timestamp()), run.id.clone()));
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Artifact {
    Checks,
    Review,
    Implementation,
    ReviewDiagnostics,
}
impl Artifact {
    pub fn filename(self) -> &'static str {
        match self {
            Self::Checks => "checks.log",
            Self::Review => "review.txt",
            Self::Implementation => "implementation.log",
            Self::ReviewDiagnostics => "review.log",
        }
    }
}
pub struct Preview {
    pub text: String,
    pub truncated: bool,
}
/// Only named text artifacts are exposed; large logs show their most recent output.
pub fn preview(data: &Path, id: &str, artifact: Artifact) -> Result<Option<Preview>> {
    const LIMIT: u64 = 48 * 1024;
    let Some(mut file) = artifact_file(data, id, artifact)? else {
        return Ok(None);
    };
    let len = file.metadata()?.len();
    let truncated = len > LIMIT;
    file.seek(SeekFrom::Start(len.saturating_sub(LIMIT)))?;
    let mut bytes = Vec::new();
    file.take(LIMIT).read_to_end(&mut bytes)?;
    Ok(Some(Preview {
        text: String::from_utf8_lossy(&bytes).into_owned(),
        truncated,
    }))
}

pub fn artifact_file(data: &Path, id: &str, artifact: Artifact) -> Result<Option<File>> {
    ident(id, 64)?;
    let dir = data.join("runs").join(id);
    let path = dir.join(artifact.filename());
    if !path.try_exists()? {
        return Ok(None);
    }
    ensure!(
        fs::symlink_metadata(&dir)?.is_dir() && fs::symlink_metadata(&path)?.is_file(),
        "无效的产物路径"
    );
    Ok(Some(File::open(&path).with_context(|| {
        format!("open {}", artifact.filename())
    })?))
}

#[cfg(test)]
#[path = "studio_tests.rs"]
mod tests;
