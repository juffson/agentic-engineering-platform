//! Fixed, bounded byte slots. The transfer never extracts paths supplied by a device.
use super::state::Lease;
use crate::git::Repo;
use aep::workspace::{git, ident};
use anyhow::{Context, Result, ensure};
use std::{
    fs::{self, File},
    io::{ErrorKind, Read, Write},
    path::Path,
    process::{Command, Stdio},
};

pub const LIMIT: usize = 24 * 1024 * 1024;
const HEADER: &[u8] = b"AEP-EVIDENCE-1\n";
const RUN_FILES: &[&str] = &[
    "evidence.json",
    "checks.log",
    "review.txt",
    "implementation.log",
    "handoff.json",
    "review-result.json",
    "review.log",
];
const SUPERVISOR_FILES: &[&str] = &["worker-error.txt", "worker.log"];

fn read_regular(root: &Path, file: &str, limit: usize) -> Result<Option<Vec<u8>>> {
    let path = root.join(file);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        metadata.is_file() && fs::symlink_metadata(root)?.is_dir(),
        "unsafe evidence artifact {file}"
    );
    ensure!(
        metadata.len() <= limit as u64,
        "evidence exceeds transfer limit"
    );
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= limit, "evidence exceeds transfer limit");
    Ok(Some(bytes))
}

fn push(packet: &mut Vec<u8>, data: Option<Vec<u8>>) -> Result<()> {
    let len = data.as_ref().map_or(0, Vec::len);
    ensure!(
        packet.len() + 4 + len <= LIMIT,
        "evidence exceeds transfer limit"
    );
    let size = if data.is_some() { len as u32 } else { u32::MAX };
    packet.extend(size.to_be_bytes());
    if let Some(data) = data {
        packet.extend(data);
    }
    Ok(())
}

fn bundle(repo: &Repo, lease: &Lease, limit: usize) -> Result<Option<Vec<u8>>> {
    let reference = format!("refs/aep/runs/{}", lease.run);
    let head = git(&repo.root, &["rev-parse", "--verify", &reference]);
    if !head
        .as_ref()
        .is_ok_and(|head| head.trim() != lease.task.spec)
    {
        return Ok(None);
    }
    let mut child = Command::new("git")
        .current_dir(&repo.root)
        .args([
            "bundle",
            "create",
            "-",
            &reference,
            &format!("^{}", lease.task.spec),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut bytes = Vec::new();
    let read = child
        .stdout
        .take()
        .context("Git bundle stdout")?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes);
    if read.is_err() || bytes.len() > limit {
        let _ = child.kill();
    }
    let success = child.wait()?.success();
    read?;
    ensure!(
        bytes.len() <= limit,
        "candidate bundle exceeds transfer limit"
    );
    ensure!(
        success && !bytes.is_empty(),
        "could not build candidate Git bundle"
    );
    Ok(Some(bytes))
}

pub fn pack(repo: &Repo, journal: &Path, lease: &Lease) -> Result<Vec<u8>> {
    ident(&lease.run, 64)?;
    let data = repo.data.canonicalize().ok();
    let has_data = data.as_deref() == Some(repo.data.as_path());
    let runs = repo.data.join("runs");
    let run = runs.join(&lease.run);
    let mut packet = HEADER.to_vec();
    for file in RUN_FILES {
        let bytes = if has_data && fs::symlink_metadata(&runs).is_ok_and(|m| m.is_dir()) {
            read_regular(&run, file, LIMIT - packet.len())?
        } else {
            None
        };
        push(&mut packet, bytes)?;
    }
    for file in SUPERVISOR_FILES {
        let bytes = read_regular(journal, file, LIMIT - packet.len())?;
        push(&mut packet, bytes)?;
    }
    let memory = if has_data {
        read_regular(&repo.data, "memory.sqlite3", LIMIT - packet.len())?
    } else {
        None
    };
    push(&mut packet, memory)?;
    let candidate = bundle(repo, lease, LIMIT - packet.len())?;
    push(&mut packet, candidate)?;
    Ok(packet)
}

pub fn persist(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(path.parent().context("evidence parent")?)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist_noclobber(path)?;
    Ok(())
}

fn slots(mut packet: &[u8]) -> Result<Vec<Option<&[u8]>>> {
    ensure!(packet.len() <= LIMIT, "evidence exceeds transfer limit");
    packet = packet
        .strip_prefix(HEADER)
        .context("invalid evidence header")?;
    let count = RUN_FILES.len() + SUPERVISOR_FILES.len() + 2;
    let mut slots = Vec::with_capacity(count);
    for _ in 0..count {
        let (size, rest) = packet.split_at_checked(4).context("truncated evidence")?;
        packet = rest;
        let size = u32::from_be_bytes(size.try_into()?);
        if size == u32::MAX {
            slots.push(None);
        } else {
            let (bytes, rest) = packet
                .split_at_checked(size as usize)
                .context("truncated evidence")?;
            slots.push(Some(bytes));
            packet = rest;
        }
    }
    ensure!(packet.is_empty(), "unexpected bytes after evidence");
    Ok(slots)
}

pub fn validate(packet: &[u8]) -> Result<()> {
    slots(packet)?;
    Ok(())
}

pub fn unpack(output: &Path, run: &str, packet: &[u8]) -> Result<()> {
    ident(run, 64)?;
    let slots = slots(packet)?;
    fs::create_dir(output)?;
    let names = RUN_FILES
        .iter()
        .map(|name| ("run", *name))
        .chain(SUPERVISOR_FILES.iter().map(|name| ("supervisor", *name)))
        .chain([("", "memory.sqlite3"), ("", "candidate.bundle")]);
    for ((dir, name), data) in names.zip(slots) {
        if let Some(data) = data {
            let parent = output.join(dir);
            fs::create_dir_all(&parent)?;
            persist(&parent.join(name), data)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_has_fixed_slots_and_no_device_paths() -> Result<()> {
        let mut payload = HEADER.to_vec();
        push(&mut payload, Some(b"{}\n".to_vec()))?;
        for _ in 1..RUN_FILES.len() + SUPERVISOR_FILES.len() + 2 {
            push(&mut payload, None)?;
        }
        assert!(validate(&payload[..payload.len() - 1]).is_err());
        let dir = tempfile::tempdir()?;
        let out = dir.path().join("evidence");
        unpack(&out, "run-1", &payload)?;
        assert_eq!(fs::read(out.join("run/evidence.json"))?, b"{}\n");
        assert!(unpack(&out, "run-1", &payload).is_err());
        Ok(())
    }
}
