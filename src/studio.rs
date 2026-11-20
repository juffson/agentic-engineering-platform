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
    let start = len.saturating_sub(LIMIT);
    // Look behind the cutoff so only a split, valid character is omitted.
    // Stray continuation bytes in the retained text must still decode lossily.
    let lookbehind = start.min(3) as usize;
    file.seek(SeekFrom::Start(start - lookbehind as u64))?;
    let mut bytes = Vec::new();
    file.take(LIMIT + lookbehind as u64)
        .read_to_end(&mut bytes)?;
    let mut retained = lookbehind.min(bytes.len());
    for i in 0..retained {
        let width = match bytes[i] {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => continue,
        };
        let end = i + width;
        if end > retained && end <= bytes.len() && std::str::from_utf8(&bytes[i..end]).is_ok() {
            retained = end;
            break;
        }
    }
    Ok(Some(Preview {
        text: String::from_utf8_lossy(&bytes[retained..]).into_owned(),
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

#[cfg(test)]
mod preview_boundary_tests {
    use super::*;

    fn read(bytes: &[u8]) -> Preview {
        let data = tempfile::tempdir().unwrap();
        let run = data.path().join("runs/test");
        fs::create_dir_all(&run).unwrap();
        fs::write(run.join("checks.log"), bytes).unwrap();
        preview(data.path(), "test", Artifact::Checks)
            .unwrap()
            .unwrap()
    }

    #[test]
    fn preserves_whole_characters_and_omits_only_split_characters() {
        const LIMIT: usize = 48 * 1024;
        for character in ["é", "€", "😀"] {
            for cut in 0..character.len() {
                let mut bytes = vec![b'x'; 4];
                bytes.extend_from_slice(character.as_bytes());
                bytes.resize(4 + cut + LIMIT, b'a');
                let result = read(&bytes);
                assert!(result.truncated);
                let expected = if cut == 0 {
                    format!("{character}{}", "a".repeat(LIMIT - character.len()))
                } else {
                    "a".repeat(LIMIT - (character.len() - cut))
                };
                assert_eq!(result.text, expected);
            }
        }
    }

    #[test]
    fn retains_lossy_decoding_for_actual_malformed_bytes() {
        const LIMIT: usize = 48 * 1024;
        // Includes stray continuations, overlong encodings, surrogates,
        // out-of-range code points, and incomplete characters.
        for malformed in [
            &b"\x80\x80"[..],
            &b"\xc0\x80"[..],
            &b"\xe0\x80\x80"[..],
            &b"\xed\xa0\x80"[..],
            &b"\xf4\x90\x80\x80"[..],
            &b"\xf0\x9f"[..],
        ] {
            let short = read(malformed);
            assert!(!short.truncated);
            assert_eq!(short.text, String::from_utf8_lossy(malformed));

            for cut in 0..malformed.len() {
                let mut bytes = vec![b'x'; 4];
                bytes.extend_from_slice(malformed);
                bytes.resize(4 + cut + LIMIT, b'a');
                let result = read(&bytes);
                assert!(result.truncated);
                assert_eq!(result.text, String::from_utf8_lossy(&bytes[4 + cut..]));
            }
        }
    }
}
