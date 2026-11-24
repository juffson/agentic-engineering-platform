//! Start the detached `aep` processes that run stages and autopilot, so they survive the app.
use anyhow::{Context, Result};
use std::{
    env, fs,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// PATH for agent CLIs: GUI apps start with a minimal PATH, so add the usual install places.
pub fn path() -> Result<std::ffi::OsString> {
    let original = env::var_os("PATH").unwrap_or_default();
    let home = env::home_dir().into_iter().flat_map(|h| {
        [
            h.join(".local/bin"),
            h.join(".cargo/bin"),
            h.join(".bun/bin"),
        ]
    });
    Ok(env::join_paths(
        env::split_paths(&original)
            .chain(home)
            .chain(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"].map(PathBuf::from)),
    )?)
}

/// The `aep` executor: explicit override, bundled sibling, this repository's build, then PATH.
pub fn binary() -> Result<PathBuf> {
    if let Some(b) = env::var_os("AEP_STUDIO_RUNNER") {
        return Ok(PathBuf::from(b));
    }
    let sibling = env::current_exe()?.with_file_name("aep");
    if sibling.is_file() {
        return Ok(sibling);
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/aep");
    if repo.is_file() {
        return Ok(repo);
    }
    env::split_paths(&path()?)
        .map(|p| p.join("aep"))
        .find(|p| p.is_file())
        .context("找不到 AEP 执行器 aep：请先构建（cargo build --bin aep）或设置 AEP_STUDIO_RUNNER")
}

fn detached(args: &[&std::ffi::OsStr], log: &Path) -> Result<()> {
    let mut child = Command::new(binary()?)
        .env("PATH", path()?)
        .env_remove(aep::CONTROL_DEVICE_TOKEN)
        .env_remove(aep::CONTROL_OPERATOR_TOKEN)
        .args(args)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(fs::File::create(log)?))
        .spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Run a reserved attempt; a failed launch is recorded so the UI shows why.
pub fn stage(dir: &Path) -> Result<()> {
    let request = dir.join("request.json");
    let result = detached(
        &[
            "workflow-stage".as_ref(),
            "--request".as_ref(),
            request.as_os_str(),
        ],
        &dir.join("supervisor.log"),
    );
    if let Err(e) = &result {
        aep::project::save_json(
            dir,
            "evidence.json",
            &serde_json::json!({"status":"failed","error":format!("{e:#}")}),
        )?;
    }
    result
}

pub fn autopilot(home: &Path, project: &str, work: &str, until: usize, dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    let until = (until + 1).to_string();
    detached(
        &[
            "workflow-auto".as_ref(),
            "--home".as_ref(),
            home.as_os_str(),
            "--project".as_ref(),
            project.as_ref(),
            "--work".as_ref(),
            work.as_ref(),
            "--until".as_ref(),
            until.as_ref(),
        ],
        &dir.join("autopilot.log"),
    )
}

/// Propose a memory tidy-up in the background; the job folder receives status and proposal.
pub fn memory_tidy(home: &Path, project: &str, job: &Path) -> Result<()> {
    detached(
        &[
            "memory-tidy".as_ref(),
            "--home".as_ref(),
            home.as_os_str(),
            "--project".as_ref(),
            project.as_ref(),
            "--job".as_ref(),
            job.as_os_str(),
        ],
        &job.join("supervisor.log"),
    )
}
