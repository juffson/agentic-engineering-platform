use aep::workspace::{Workspace, git, read};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{env, fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

pub const RUN: &str = "memory-handoff";
pub const FOLLOW_UP: &str = "memory-follow-up";
pub const QUESTION: &str = "Should the conversion saturate?";
pub const DECISION: &str = "Return the exact widened value.";

#[derive(Clone, Copy)]
pub enum Clients<'a> {
    Fixture,
    Live { model: Option<&'a str> },
}

pub fn prepare(root: &Path, clients: Clients<'_>) -> Result<Workspace> {
    let repo = root.join("repo");
    let remote = root.join("remote.git");
    let bin = root.join("bin");
    fs::create_dir(&repo)?;
    fs::create_dir(&remote)?;
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
    let spec = match clients {
        Clients::Fixture => include_str!("handoff/spec.md"),
        Clients::Live { .. } => include_str!("handoff/live-spec.md"),
    };
    for (path, content) in [
        ("specs/handoff/spec.md", spec),
        ("specs/handoff/task.json", include_str!("handoff/task.json")),
        ("normalize.rs", include_str!("handoff/normalize.rs")),
        ("acceptance.rs", include_str!("handoff/acceptance.rs")),
    ] {
        fs::write(repo.join(path), content)?;
    }
    git(&repo, &["add", "."])?;
    git(&repo, &["commit", "-qm", "Pin the handoff spec"])?;

    if matches!(clients, Clients::Fixture) {
        fs::create_dir(&bin)?;
        for (name, script) in [
            ("codex", include_str!("handoff/codex.sh")),
            ("claude", include_str!("handoff/claude.sh")),
        ] {
            let path = bin.join(name);
            fs::write(&path, script)?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
    }
    aep(root, clients, RUN, &["dispatch", "--task", "handoff"])?;
    Workspace::open(&repo)
}

pub fn run(root: &Path, clients: Clients<'_>, id: &str) -> Result<()> {
    let mut args = vec![
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
        match clients {
            Clients::Fixture => "30",
            Clients::Live { .. } => "220",
        },
    ];
    if let Clients::Live { model: Some(model) } = clients {
        args.extend(["--codex-model", model]);
    }
    aep(root, clients, id, &args).with_context(|| format!("execute {id}"))
}

fn aep(root: &Path, clients: Clients<'_>, id: &str, args: &[&str]) -> Result<()> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../Cargo.toml");
    let mut command = Command::new("cargo");
    command
        .args(["run", "--quiet", "--locked", "--manifest-path"])
        .arg(&manifest)
        .args(["--bin", "aep", "--", "--repo"])
        .arg(root.join("repo"))
        .args(args);
    if matches!(clients, Clients::Fixture) {
        let original_path = env::var_os("PATH").unwrap_or_default();
        let path = env::join_paths(
            std::iter::once(root.join("bin")).chain(env::split_paths(&original_path)),
        )?;
        command
            .env("PATH", path)
            .env("TMPDIR", root)
            .env("AEP_FIXTURE_RUN", id)
            .env("AEP_FIXTURE_QUESTION", QUESTION)
            .env("AEP_FIXTURE_DECISION", DECISION);
    }
    let output = command
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

pub fn verify_reference(ws: &Workspace, next: &Value, reference: &str) -> Result<()> {
    let folder = ws.data.join("runs").join(FOLLOW_UP);
    let handoff = read(&folder.join("handoff.json"))?;
    let review = read(&folder.join("review-result.json"))?;
    ensure!(
        handoff["note"]
            .as_str()
            .is_some_and(|s| s.contains(reference))
            && handoff["question"] == "",
        "Codex did not acknowledge the decision reference and close the question"
    );
    ensure!(
        review["structured_output"]["report"]
            .as_str()
            .is_some_and(|s| s.contains(reference)),
        "Claude's report did not quote the decision reference"
    );
    let spec = next["spec_sha"].as_str().context("spec SHA")?;
    let candidate = next["candidate_sha"].as_str().context("candidate SHA")?;
    let diff = git(&ws.root, &["diff", "--no-ext-diff", spec, candidate])?;
    ensure!(
        !diff.contains(reference) && !include_str!("handoff/live-spec.md").contains(reference),
        "decision reference also appeared in the spec or code diff"
    );
    Ok(())
}

pub fn export_runs(ws: &Workspace, dest: &Path) -> Result<()> {
    for id in [RUN, FOLLOW_UP] {
        let source = ws.data.join("runs").join(id);
        if !source.exists() {
            continue;
        }
        let output = dest.join(id);
        fs::create_dir(&output)?;
        for file in [
            "evidence.json",
            "handoff.json",
            "checks.log",
            "review.txt",
            "review-result.json",
        ] {
            let path = source.join(file);
            if path.try_exists()? {
                fs::copy(path, output.join(file))?;
            }
        }
    }
    Ok(())
}

pub fn studio_runner(root: &Path) -> Result<aep_studio::Runner> {
    let bin = root.join("bin");
    for (name, script) in [
        ("codex", include_str!("handoff/studio-codex.sh")),
        ("claude", include_str!("handoff/studio-claude.sh")),
        ("git", include_str!("handoff/studio-git.sh")),
        ("gh", include_str!("handoff/studio-gh.sh")),
    ] {
        let path = bin.join(name);
        fs::write(&path, script)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let target = source.join("target");
    let output = Command::new("cargo")
        .current_dir(&source)
        .args([
            "build",
            "--locked",
            "--quiet",
            "--bin",
            "aep",
            "--target-dir",
        ])
        .arg(&target)
        .output()?;
    ensure!(
        output.status.success(),
        "build AEP runner: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(aep_studio::Runner {
        binary: Some(target.join("debug/aep")),
        client_bin: Some(bin),
    })
}
