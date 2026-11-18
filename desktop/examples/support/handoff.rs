use aep::workspace::{Workspace, git};
use anyhow::{Context, Result, ensure};
use std::{env, fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

pub const RUN: &str = "memory-handoff";
pub const FOLLOW_UP: &str = "memory-follow-up";
pub const QUESTION: &str = "Should the conversion saturate?";
pub const DECISION: &str = "Return the exact widened value.";

pub fn start(root: &Path) -> Result<Workspace> {
    let repo = root.join("repo");
    let remote = root.join("remote.git");
    let bin = root.join("bin");
    fs::create_dir(&repo)?;
    fs::create_dir(&remote)?;
    fs::create_dir(&bin)?;
    git(&remote, &["init", "-q", "--bare", "--template="])?;
    git(&repo, &["init", "-q", "--template="])?;
    for (key, value) in [
        ("user.name", "AEP UI Fixture"),
        ("user.email", "fixture@example.invalid"),
        ("commit.gpgsign", "false"),
        ("core.hooksPath", "/dev/null"),
    ] {
        git(&repo, &["config", "--local", key, value])?;
    }
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            remote.to_str().context("remote path")?,
        ],
    )?;
    fs::create_dir_all(repo.join("specs/handoff"))?;
    for (path, content) in [
        ("specs/handoff/spec.md", include_str!("handoff/spec.md")),
        ("specs/handoff/task.json", include_str!("handoff/task.json")),
        ("normalize.rs", include_str!("handoff/normalize.rs")),
        ("acceptance.rs", include_str!("handoff/acceptance.rs")),
    ] {
        fs::write(repo.join(path), content)?;
    }
    git(&repo, &["add", "."])?;
    git(&repo, &["commit", "-qm", "Pin the handoff spec"])?;

    for (name, script) in [
        ("codex", include_str!("handoff/codex.sh")),
        ("claude", include_str!("handoff/claude.sh")),
    ] {
        let path = bin.join(name);
        fs::write(&path, script)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    aep(root, RUN, &["dispatch", "--task", "handoff"])?;
    run(root, RUN)?;
    Workspace::open(&repo)
}

pub fn run(root: &Path, id: &str) -> Result<()> {
    aep(
        root,
        id,
        &[
            "run",
            "--task",
            "handoff",
            "--implementer",
            "codex",
            "--reviewer",
            "claude",
            "--shared-memory",
            "--run-id",
            id,
            "--timeout",
            "30",
        ],
    )
}

fn aep(root: &Path, id: &str, args: &[&str]) -> Result<()> {
    let original_path = env::var_os("PATH").unwrap_or_default();
    let path =
        env::join_paths(std::iter::once(root.join("bin")).chain(env::split_paths(&original_path)))?;
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../Cargo.toml");
    let output = Command::new("cargo")
        .args(["run", "--quiet", "--locked", "--manifest-path"])
        .arg(&manifest)
        .args(["--bin", "aep", "--", "--repo"])
        .arg(root.join("repo"))
        .args(args)
        .env("PATH", &path)
        .env("TMPDIR", root)
        .env("AEP_FIXTURE_RUN", id)
        .env("AEP_FIXTURE_QUESTION", QUESTION)
        .env("AEP_FIXTURE_DECISION", DECISION)
        .output()
        .with_context(|| format!("run aep {}", args[0]))?;
    ensure!(
        output.status.success(),
        "aep {} failed:\n{}\n{}",
        args[0],
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
