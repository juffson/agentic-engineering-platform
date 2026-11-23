use aep::workflow::{Client, Role, Template};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
pub fn implementation_stage(template: Template) -> Option<usize> {
    let stages = template.stages();
    let start = stages.len().checked_sub(4)?;
    stages[start..]
        .iter()
        .map(|s| s.role)
        .eq([Role::Build, Role::Test, Role::Accept, Role::Deliver])
        .then_some(start)
}

pub fn storage(home: &Path, project: &str, work: &str) -> Result<PathBuf> {
    aep::workspace::ident(project, 64)?;
    aep::workspace::ident(work, 64)?;
    Ok(home
        .join("workflows")
        .join(project)
        .join(work)
        .join("studio-deliveries"))
}
#[derive(Clone, Deserialize, Serialize)]
pub struct Profile {
    pub client: Client,
    pub model: String,
    pub brief: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Plan {
    pub repository: PathBuf,
    pub work: String,
    pub stage: usize,
    pub task: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_sha: Option<String>,
    pub first: Models,
    pub second: Models,
    #[serde(default)]
    pub decision: String,
    pub test: Profile,
    pub accept: Profile,
    pub deliver: Profile,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Models {
    pub implementer: String,
    pub implementation_model: String,
    pub reviewer: String,
    pub review_model: String,
}

impl Models {
    pub fn verify_receipt(&self, ev: &serde_json::Value) -> Result<()> {
        let model = if self.implementer == "codex" {
            "codex_model"
        } else {
            "zcode_model"
        };
        ensure!(
            ev["implementation_source"] == self.implementer
                && ev["reviewer"] == self.reviewer
                && ev["request"][model] == self.implementation_model
                && ev["request"]["reviewer_model"] == self.review_model
                && ev["check_exit_code"] == 0
                && ev["review_status"] == "findings_recorded"
                && ev["blocking_findings"]
                    .as_array()
                    .is_some_and(Vec::is_empty)
                && ev["acceptance"].as_array().is_some_and(|a| !a.is_empty()
                    && a.iter()
                        .all(|c| c["status"] == "passed" && c["exit_code"] == 0)),
            "接力的客户端、模型、验收或审查回执不符"
        );
        Ok(())
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            matches!(self.implementer.as_str(), "codex" | "zcode")
                && matches!(self.reviewer.as_str(), "claude" | "zcode")
                && !self.implementation_model.trim().is_empty()
                && !self.review_model.trim().is_empty(),
            "请指定两个接力角色支持的客户端与模型"
        );
        Ok(())
    }
}
use aep::{project::save_json, workspace::read};
use std::{
    fs,
    fs::{File, TryLockError},
    os::unix::process::CommandExt,
    process::{Command, Stdio},
};

pub(crate) fn jobs(root: &Path) -> Result<Vec<PathBuf>> {
    if !root.is_dir() {
        return Ok(vec![]);
    }
    let mut paths = vec![];
    for e in fs::read_dir(root)? {
        let path = e?.path();
        if path.join("launch.json").is_file() {
            paths.push(path);
        }
    }
    paths.sort();
    paths.reverse();
    Ok(paths)
}
fn launch_lock(root: &Path) -> Result<File> {
    Ok(File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join("launcher.lock"))?)
}
pub(crate) fn running(root: &Path) -> Result<bool> {
    match launch_lock(root)?.try_lock() {
        Ok(()) => Ok(false),
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(e)) => Err(e.into()),
    }
}
pub(crate) fn launch(
    root: &Path,
    plan: &Plan,
    resume: Option<&Path>,
    runner: &crate::Runner,
) -> Result<PathBuf> {
    fs::create_dir_all(root)?;
    let lock = launch_lock(root)?;
    lock.try_lock().context("此事项的全流程正在运行")?;
    let exe = std::env::current_exe()?;
    let bundled = exe.with_file_name("aep-stage-lifecycle");
    let helper = if bundled.is_file() {
        bundled
    } else if exe.file_stem().is_some_and(|s| s == "ui_debug") {
        exe
    } else {
        exe.with_file_name("examples").join("ui_debug")
    };
    ensure!(helper.is_file(), "缺少全流程执行器，请重新构建 Studio");
    let binary = runner.binary()?;
    let dir = if let Some(path) = resume {
        ensure!(path.parent() == Some(root), "执行目录不属于该事项");
        path.to_owned()
    } else {
        if let Some(prev) = jobs(root)?.first() {
            ensure!(
                read(&prev.join("output/verified.json")).is_ok_and(|v| v["verified"] == true),
                "请先继续上一条全流程"
            );
        }
        let directory = root.join(format!(
            "{}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis(),
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir(&directory)?;
        save_json(&directory, "launch.json", plan)?;
        directory
    };
    let log = File::options()
        .append(true)
        .create(true)
        .open(dir.join("runner.log"))?;
    let mut child = Command::new(helper)
        .arg("--live-stage-lifecycle")
        .arg(dir.join("launch.json"))
        .arg("--output-dir")
        .arg(dir.join("output"))
        .env("AEP_STUDIO_RUNNER", binary)
        .env("PATH", runner.path()?)
        .current_dir(&plan.repository)
        .process_group(0)
        .stdin(Stdio::from(lock))
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;
    // The child retains the launch lock on stdin across Studio restarts.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(dir)
}
pub(crate) fn asset(root: &Path, relative: &str) -> Result<PathBuf> {
    let rel = Path::new(relative);
    ensure!(
        !rel.as_os_str().is_empty()
            && rel
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
        "无效证据路径"
    );
    let path = root.join(rel).canonicalize()?;
    ensure!(
        path.starts_with(root.canonicalize()?) && path.is_file(),
        "证据不属于此执行"
    );
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concurrent_windows_share_a_single_launch_lock() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let owner = launch_lock(dir.path())?;
        owner.try_lock()?;
        assert!(running(dir.path())?);
        assert!(launch_lock(dir.path())?.try_lock().is_err());
        drop(owner);
        assert!(!running(dir.path())?);
        Ok(())
    }
    #[test]
    fn evidence_links_stay_inside_the_selected_run() -> Result<()> {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir()?;
        let output = dir.path().join("output");
        fs::create_dir(&output)?;
        fs::write(output.join("verified.json"), "{}")?;
        fs::write(dir.path().join("outside"), "private")?;
        symlink(dir.path().join("outside"), output.join("link"))?;
        assert!(asset(&output, "verified.json").is_ok());
        assert!(asset(&output, "../outside").is_err());
        assert!(asset(&output, "link").is_err());
        Ok(())
    }
}
