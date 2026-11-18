use super::*;
use crate::memory::Key;
use serde_json::json;
use std::os::unix::fs::symlink;

fn workspace(root: &Path) -> Workspace {
    Workspace {
        root: root.to_owned(),
        data: root.join(".git/aep"),
    }
}

#[test]
fn browsing_an_empty_workspace_creates_no_state() {
    let root = tempfile::tempdir().unwrap();
    let ws = workspace(root.path());
    let snap = Snapshot::load(&ws);
    assert!(snap.runs.is_empty() && snap.notes.is_empty() && snap.issues.is_empty());
    assert!(!ws.data.exists());
}

#[test]
fn inspection_preserves_historical_notes_and_run_evidence() {
    let root = tempfile::tempdir().unwrap();
    let ws = workspace(root.path());
    let mut memory = Memory::open(&ws.data).unwrap();
    let scope = Key::new("task", "spec", "");
    let old = memory.add(&scope, "decision", "first", None).unwrap();
    let new = memory
        .add(&scope, "decision", "revised", Some(old))
        .unwrap();
    for (id, date) in [
        ("older", "2026-01-02T09:00:00+08:00"),
        ("newer", "2026-01-02T02:00:00Z"),
    ] {
        let dir = ws.data.join("runs").join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("evidence.json"),json!({"status":"running","started_at":date,"request":{},"memory_inputs":{"reviewer":[old]}}).to_string()).unwrap();
        File::create(dir.join("owner.lock")).unwrap();
    }
    let bad = ws.data.join("runs/bad");
    fs::create_dir(&bad).unwrap();
    fs::write(bad.join("evidence.json"), b"{").unwrap();
    let snap = Snapshot::load(&ws);
    assert_eq!(snap.issues.len(), 1);
    assert_eq!(
        snap.runs.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        ["newer", "older"]
    );
    assert_eq!(snap.runs[0].text("status"), "needs_reconciliation");
    assert_eq!(snap.runs[0].record["memory_inputs"]["reviewer"][0], old);
    assert_eq!(snap.notes[0]["replaces"], old);
    assert_eq!(snap.notes[1]["successor"], new);
    assert_eq!(snap.notes[1]["body"], "first");
    let raw = crate::workspace::read(&ws.data.join("runs/newer/evidence.json")).unwrap();
    assert_eq!(raw["status"], "running");
}

#[test]
fn log_preview_is_bounded_and_rejects_indirect_paths() {
    let root = tempfile::tempdir().unwrap();
    let ws = workspace(root.path());
    let dir = ws.data.join("runs/one");
    fs::create_dir_all(&dir).unwrap();
    let log = dir.join("checks.log");
    fs::write(&log, format!("{}END", "a".repeat(60 * 1024))).unwrap();
    let p = preview(&ws.data, "one", Artifact::Checks).unwrap().unwrap();
    assert!(p.truncated && p.text.ends_with("END"));
    assert_eq!(p.text.len(), 48 * 1024);
    assert!(
        preview(&ws.data, "one", Artifact::Review)
            .unwrap()
            .is_none()
    );
    symlink(log, dir.join("review.txt")).unwrap();
    assert!(preview(&ws.data, "one", Artifact::Review).is_err());
    assert!(preview(&ws.data, "../one", Artifact::Review).is_err());
}
