use crate::{
    Runner,
    lifecycle::{Branch, Draft},
    relay::Task,
};
use aep::workspace::{Workspace, ident};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::{self, File, TryLockError},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
};

const PREVIEW_BYTES: u64 = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Gate {
    pub candidate: String,
    pub pr: Option<u64>,
}

impl Gate {
    fn command(&self) -> String {
        if let Some(pr) = self.pr {
            format!("merge {pr} {}\n", self.candidate)
        } else {
            format!("publish {}\n", self.candidate)
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub(crate) struct Progress {
    pub remote: bool,
    pub ticket: String,
    pub stage: String,
    pub repository: String,
    pub task: String,
    pub branch: String,
    pub candidate: String,
    pub old_run: String,
    pub recovered_run: String,
    pub worker: String,
    pub pr: Option<u64>,
    pub archive: bool,
    pub reason: String,
    pub exit_code: Option<i32>,
}

fn hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

impl Progress {
    fn task_is_valid(&self) -> bool {
        if self.remote {
            return ident(&self.task, 63).is_ok()
                && self.ticket.starts_with("https-task-")
                && ident(&self.ticket, 63).is_ok()
                && !self.branch.is_empty();
        }
        self.task
            .strip_prefix("https-drill-")
            .is_some_and(|id| hex(id, 16))
            && self.branch == format!("test/{}", self.task)
    }

    pub fn gate(&self) -> Option<Gate> {
        if self.exit_code.is_some() || !self.task_is_valid() || !hex(&self.candidate, 40) {
            return None;
        }
        let pr = match self.stage.as_str() {
            "publish_confirmation" if self.pr.is_none() => None,
            "merge_confirmation" => Some(self.pr.filter(|pr| *pr > 0)?),
            _ => return None,
        };
        Some(Gate {
            candidate: self.candidate.clone(),
            pr,
        })
    }

    pub fn pr_url(&self) -> Option<String> {
        let (owner, name) = self.repository.split_once('/')?;
        if [owner, name].iter().any(|segment| {
            segment.is_empty()
                || matches!(*segment, "." | "..")
                || !segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        }) {
            return None;
        }
        Some(format!(
            "https://github.com/{owner}/{name}/pull/{}",
            self.pr?
        ))
    }
}

#[derive(Clone, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct Connection {
    pub endpoint: String,
    pub ca: Option<PathBuf>,
    pub operator_file: PathBuf,
}

#[derive(Deserialize, Serialize)]
struct Request {
    #[serde(default)]
    ticket: String,
    task: Task,
    branch: Branch,
    #[serde(flatten)]
    connection: Connection,
}

pub(crate) enum TaskSource {
    Existing(Task),
    Draft(Draft),
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Artifact {
    #[default]
    Log,
    Diff,
    Checks,
    Review,
    FirstRun,
    RecoveryRun,
    CiLog,
    Result,
}

fn read_json<T: serde::de::DeserializeOwned>(path: impl AsRef<Path>) -> Option<T> {
    serde_json::from_reader(File::open(path).ok()?).ok()
}

fn preview(path: &Path, tail: bool) -> Option<String> {
    let mut file = File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let length = file.metadata().ok()?.len();
    if tail {
        file.seek(SeekFrom::Start(length.saturating_sub(PREVIEW_BYTES)))
            .ok()?;
    }
    let mut bytes = Vec::new();
    file.take(PREVIEW_BYTES).read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    Some(if length <= PREVIEW_BYTES {
        text.into()
    } else if tail {
        format!("… 最近日志 …\n{text}")
    } else {
        format!("{text}\n… 可在证据目录查看完整文件 …")
    })
}

pub(crate) struct Entry {
    pub root: PathBuf,
    pub progress: Progress,
    pub busy: bool,
}

pub(crate) struct Session {
    pub root: PathBuf,
    pub source: PathBuf,
    remote: bool,
    pub busy: bool,
    pub progress: Progress,
    pub checks: Vec<Value>,
    pub selected: Artifact,
    pub text: String,
    pub error: Option<String>,
    pub acknowledged: Option<Gate>,
    submitted: Option<Gate>,
    child: Option<Child>,
    approval: Option<ChildStdin>,
}

impl Session {
    fn new(root: PathBuf, source: PathBuf) -> Self {
        Self {
            root,
            source,
            remote: false,
            busy: false,
            progress: Progress::default(),
            checks: Vec::new(),
            selected: Artifact::Log,
            text: String::new(),
            error: None,
            acknowledged: None,
            submitted: None,
            child: None,
            approval: None,
        }
    }

    pub fn launch(source: &Workspace, runner: &Runner) -> Result<Self> {
        let script = source.root.join("deploy/pilot/live.sh");
        ensure!(
            script.is_file()
                && script
                    .canonicalize()?
                    .starts_with(source.root.canonicalize()?),
            "请打开包含 deploy/pilot/live.sh 的 AEP 源码仓库"
        );
        let session = Self::allocate(source)?;
        let mut command = Command::new("bash");
        command
            .arg(script)
            .arg("--studio-session")
            .arg(&session.root);
        session.spawn(source, runner, command, None)
    }

    pub fn launch_remote(
        source: &Workspace,
        runner: &Runner,
        task: TaskSource,
        branch: Branch,
        connection: Connection,
    ) -> Result<Self> {
        let mut session = Self::allocate(source)?;
        session.remote = true;
        let task = match task {
            TaskSource::Existing(task) => task,
            TaskSource::Draft(draft) => {
                draft.write_files(&session.root.join("draft"))?;
                Task {
                    name: draft.name,
                    sha: String::new(),
                }
            }
        };
        let request = Request {
            ticket: format!("https-task-{}", uuid::Uuid::new_v4().simple()),
            task,
            branch,
            connection,
        };
        serde_json::to_writer(File::create(session.root.join("request.json"))?, &request)?;
        session.launch_saved(source, runner, false)
    }

    pub fn resume(mut self, source: &Workspace, runner: &Runner) -> Result<Self> {
        self.refresh();
        ensure!(self.can_resume(), "任务正在运行、已完成，或缺少派发参数");
        let retry = self.retry_needed();
        self.launch_saved(source, runner, retry)
    }

    fn launch_saved(mut self, source: &Workspace, runner: &Runner, retry: bool) -> Result<Self> {
        let binary = runner.binary()?.canonicalize()?;
        let owner = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.root.join("operator.lock"))?;
        owner.try_lock().context("另一个 Studio 正在跟踪此任务")?;
        let pause = self.root.join("pause-request");
        if pause.exists() {
            fs::remove_file(pause)?;
        }
        let script = self.root.join("remote-task.sh");
        fs::write(&script, include_str!("../../deploy/pilot/remote-task.sh"))?;
        fs::write(
            self.root.join("live-github.sh"),
            include_str!("../../deploy/pilot/live-github.sh"),
        )?;
        let mut command = Command::new("bash");
        command
            .arg(script)
            .arg(&self.root)
            .arg(binary)
            .arg(&source.root)
            .arg(std::process::id().to_string())
            .arg(if retry { "retry" } else { "observe" });
        self.error = None;
        self.acknowledged = None;
        self.submitted = None;
        self.spawn(source, runner, command, Some(owner))
    }

    fn allocate(source: &Workspace) -> Result<Self> {
        let parent = source.data.join("https-studio");
        fs::create_dir_all(&parent)?;
        let root = tempfile::Builder::new()
            .prefix("studio-")
            .tempdir_in(parent.canonicalize()?)?
            .keep();
        Ok(Self::new(root, source.root.clone()))
    }

    fn spawn(
        mut self,
        source: &Workspace,
        runner: &Runner,
        mut command: Command,
        owner: Option<File>,
    ) -> Result<Self> {
        let log = File::options()
            .create(true)
            .append(true)
            .open(self.root.join("studio.log"))?;
        let stdout = match owner {
            Some(owner) => owner,
            None => log.try_clone()?,
        };
        match command
            .current_dir(&source.root)
            .env("PATH", runner.path()?)
            .env("GH_PROMPT_DISABLED", "1")
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(log))
            .spawn()
        {
            Ok(mut child) => {
                self.approval = child.stdin.take();
                self.child = Some(child);
            }
            Err(error) => {
                let reason = format!("无法启动 HTTPS 试点：{error}");
                writeln!(
                    File::options()
                        .append(true)
                        .open(self.root.join("studio.log"))?,
                    "{reason}"
                )?;
                self.error = Some(reason);
            }
        }
        self.refresh_progress();
        Ok(self)
    }

    fn roots(source: &Workspace) -> Vec<PathBuf> {
        let mut roots = fs::read_dir(source.data.join("https-studio"))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| {
                entry.file_name().to_string_lossy().starts_with("studio-")
                    && entry.file_type().is_ok_and(|kind| kind.is_dir())
            })
            .collect::<Vec<_>>();
        roots.sort_by_cached_key(|entry| {
            std::cmp::Reverse(entry.metadata().and_then(|m| m.modified()).ok())
        });
        roots.into_iter().map(|entry| entry.path()).collect()
    }

    pub fn entries(source: &Workspace) -> Vec<Entry> {
        Self::roots(source)
            .into_iter()
            .filter_map(|root| {
                let (progress, remote) = Self::read_progress(&root);
                remote.then(|| Entry {
                    busy: Self::locked(&root),
                    root,
                    progress,
                })
            })
            .collect()
    }

    pub fn saved_connection(source: &Workspace) -> Option<Connection> {
        Self::roots(source).into_iter().find_map(|root| {
            read_json::<Request>(root.join("request.json")).map(|request| request.connection)
        })
    }

    pub fn latest(source: &Workspace) -> Option<PathBuf> {
        Self::roots(source).into_iter().next()
    }

    pub fn open(source: &Workspace, root: PathBuf) -> Option<Self> {
        if root.parent()? != source.data.join("https-studio") || !root.is_dir() {
            return None;
        }
        let mut session = Self::new(root, source.root.clone());
        session.refresh();
        if session.archive().is_some() {
            session.select(Artifact::Result);
        }
        Some(session)
    }

    fn locked(root: &Path) -> bool {
        File::open(root.join("operator.lock"))
            .ok()
            .is_some_and(|file| matches!(file.try_lock_shared(), Err(TryLockError::WouldBlock)))
    }

    fn read_progress(root: &Path) -> (Progress, bool) {
        let mut progress = read_json::<Progress>(root.join("state.json")).unwrap_or_default();
        let request = read_json::<Request>(root.join("request.json"));
        if let Some(request) = &request {
            progress.remote = true;
            if progress.task.is_empty() {
                progress.task.clone_from(&request.task.name);
                progress.branch.clone_from(&request.branch.name);
                progress.ticket.clone_from(&request.ticket);
            }
        }
        (progress, request.is_some())
    }

    fn refresh_progress(&mut self) {
        (self.progress, self.remote) = Self::read_progress(&self.root);
        self.busy = self.remote && Self::locked(&self.root);
    }

    pub fn can_resume(&self) -> bool {
        self.remote
            && !self.active()
            && !self.busy
            && (self.progress.exit_code != Some(0) || self.archive().is_none())
    }

    pub fn retry_needed(&self) -> bool {
        self.error.is_some()
            || self
                .progress
                .exit_code
                .is_some_and(|exit| !matches!(exit, 0 | 3 | 4))
    }

    pub fn active(&self) -> bool {
        self.child.is_some()
    }

    pub fn gate(&self) -> Option<Gate> {
        self.approval.as_ref()?;
        self.progress
            .gate()
            .filter(|gate| Some(gate) != self.submitted.as_ref())
    }

    pub fn confirm(&mut self) -> Result<()> {
        let shown = self.gate().context("当前没有可确认的操作")?;
        let current: Progress =
            read_json(self.root.join("state.json")).context("无法读取确认状态")?;
        ensure!(
            current.gate().as_ref() == Some(&shown)
                && self.acknowledged.as_ref() == Some(&shown)
                && current.task == self.progress.task
                && current.repository == self.progress.repository
                && current.branch == self.progress.branch
                && current.ticket == self.progress.ticket,
            "候选、目标或确认阶段已变化，请重新核对"
        );
        self.acknowledged = None;
        let mut pipe = self.approval.take().context("试点确认通道已关闭")?;
        pipe.write_all(shown.command().as_bytes())
            .context("无法写入试点确认")?;
        self.approval = Some(pipe);
        self.submitted = Some(shown);
        Ok(())
    }

    pub fn stop_at_gate(&mut self) {
        if self.gate().is_some() {
            self.approval.take();
            self.acknowledged = None;
        }
    }

    fn run_dir(&self) -> Option<PathBuf> {
        self.progress
            .task_is_valid()
            .then(|| self.root.join(&self.progress.task))
    }

    pub fn archive(&self) -> Option<PathBuf> {
        let path = self.run_dir()?.join("evidence.tar.gz");
        (self.progress.archive && path.is_file()).then_some(path)
    }

    pub fn evidence_dir(&self) -> PathBuf {
        self.run_dir()
            .map(|dir| dir.join("evidence"))
            .filter(|dir| dir.is_dir())
            .unwrap_or_else(|| self.root.clone())
    }

    fn artifact_path(&self, artifact: Artifact) -> Option<PathBuf> {
        if artifact == Artifact::Log {
            return Some(self.root.join("studio.log"));
        }
        let dir = self.run_dir()?.join("evidence");
        let run = |id: &str, file: &str| {
            if !hex(id, 32) {
                return None;
            }
            ["worker", "partial-worker"]
                .into_iter()
                .map(|part| dir.join(part).join("runs").join(id).join(file))
                .find(|path| path.is_file())
        };
        match artifact {
            Artifact::Diff => Some(dir.join("candidate.diff")),
            Artifact::Checks => run(&self.progress.recovered_run, "checks.log"),
            Artifact::Review => run(&self.progress.recovered_run, "review.txt"),
            Artifact::FirstRun => run(&self.progress.old_run, "evidence.json"),
            Artifact::RecoveryRun => run(&self.progress.recovered_run, "evidence.json"),
            Artifact::CiLog => Some(dir.join("github-acceptance.log")),
            Artifact::Result => Some(dir.join("result.json")),
            Artifact::Log => unreachable!(),
        }
    }

    pub fn has(&self, artifact: Artifact) -> bool {
        self.artifact_path(artifact)
            .is_some_and(|path| path.is_file())
    }

    pub fn refresh(&mut self) {
        // A stopped child has finished writing the terminal state and archive.
        let exit = self
            .child
            .as_mut()
            .and_then(|child| match child.try_wait() {
                Ok(status) => status,
                Err(error) => {
                    self.error = Some(format!("读取试点进程状态失败：{error}"));
                    None
                }
            });
        if exit.is_some() {
            self.child.take();
            self.approval.take();
        }
        self.refresh_progress();
        if let Some(exit) = exit {
            self.progress.exit_code = Some(exit.code().unwrap_or(1));
        }
        if self.gate() != self.acknowledged {
            self.acknowledged = None;
        }
        self.checks = self
            .run_dir()
            .and_then(|dir| read_json::<Value>(dir.join("evidence/github-ci.json")))
            .filter(|pr| pr["headRefOid"] == self.progress.candidate)
            .and_then(|mut pr| pr["statusCheckRollup"].take().as_array().cloned())
            .unwrap_or_default();
        self.text = self
            .artifact_path(self.selected)
            .and_then(|path| preview(&path, self.selected == Artifact::Log))
            .unwrap_or_default();
    }

    pub fn select(&mut self, artifact: Artifact) {
        self.selected = artifact;
        self.refresh();
    }

    pub fn failure(&self) -> Option<String> {
        if let Some(error) = &self.error {
            return Some(error.clone());
        }
        let exit = self.progress.exit_code.filter(|code| *code != 0)?;
        let run_error = [Artifact::RecoveryRun, Artifact::FirstRun]
            .into_iter()
            .filter_map(|artifact| read_json::<Value>(self.artifact_path(artifact)?))
            .filter(|run| run["status"] == "failed")
            .find_map(|run| run["error"].as_str().map(str::to_owned));
        let reason = if self.progress.reason.is_empty() {
            None
        } else {
            Some(self.progress.reason.clone())
        };
        reason
            .into_iter()
            .chain(run_error)
            .reduce(|reason, error| format!("{reason}\n{error}"))
            .or_else(|| {
                preview(&self.root.join("studio.log"), true).filter(|log| !log.trim().is_empty())
            })
            .or_else(|| Some(format!("试点进程退出（状态码 {exit}）")))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.remote && self.active() {
            let _ = File::create(self.root.join("pause-request"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn repository_catalog_preserves_a_failed_task_and_guards_resumption() -> Result<()> {
        use std::io::{BufRead, BufReader};

        let dir = tempfile::tempdir()?;
        let source = Workspace {
            root: dir.path().into(),
            data: dir.path().join("aep"),
        };
        let other = Workspace {
            root: dir.path().into(),
            data: dir.path().join("other"),
        };
        let root = source.data.join("https-studio/studio-example");
        fs::create_dir_all(&root)?;
        serde_json::to_writer(
            File::create(root.join("request.json"))?,
            &json!({
                "ticket": "https-task-0123456789abcdef0123456789abcdef",
                "task": {"name":"task-one","sha":"1".repeat(40)},
                "branch": {"name":"test/example","sha":"2".repeat(40)},
                "endpoint":"https://control.example.org",
                "ca":null,
                "operator_file":"/operator"
            }),
        )?;
        fs::write(
            root.join("state.json"),
            br#"{"stage":"remote_checking","exit_code":1,"reason":"acceptance exited 44"}"#,
        )?;
        let entries = Session::entries(&source);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].progress.task, "task-one");
        assert_eq!(entries[0].progress.branch, "test/example");
        assert_eq!(entries[0].progress.reason, "acceptance exited 44");
        assert!(Session::entries(&other).is_empty());
        assert!(Session::open(&other, root.clone()).is_none());

        let owner = File::create(root.join("operator.lock"))?;
        owner.try_lock()?;
        let mut child = Command::new("bash")
            .args(["-c", "exec 4>&1; echo ready >&2; read -r line"])
            .stdin(Stdio::piped())
            .stdout(Stdio::from(owner))
            .stderr(Stdio::piped())
            .spawn()?;
        let mut ready = String::new();
        BufReader::new(child.stderr.take().context("lock holder stderr")?).read_line(&mut ready)?;
        let mut session = Session::open(&source, root).context("saved task")?;
        let was_locked = session.busy && !session.can_resume();
        child
            .stdin
            .take()
            .context("lock holder stdin")?
            .write_all(b"released\n")?;
        assert!(child.wait()?.success());
        assert_eq!(ready.trim(), "ready");
        assert!(was_locked);
        session.refresh();
        assert!(session.can_resume());
        assert!(session.retry_needed());
        Ok(())
    }

    #[test]
    fn approval_uses_the_current_full_sha_and_stage() -> Result<()> {
        let root = tempfile::tempdir()?;
        let state = root.path().join("state.json");
        let mut pending = json!({
            "stage": "publish_confirmation",
            "repository": "owner/repo",
            "task": "https-drill-0123456789abcdef",
            "branch": "test/https-drill-0123456789abcdef",
            "candidate": "1".repeat(40)
        });
        fs::write(&state, serde_json::to_vec(&pending)?)?;
        let mut child = Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let mut session = Session::new(root.path().into(), root.path().into());
        session.approval = child.stdin.take();
        session.refresh();

        assert!(session.confirm().is_err());
        session.acknowledged = session.gate();
        pending["candidate"] = json!("2".repeat(40));
        fs::write(&state, serde_json::to_vec(&pending)?)?;
        assert!(session.confirm().is_err());
        session.refresh();
        assert!(session.acknowledged.is_none());
        session.acknowledged = session.gate();
        session.confirm()?;
        assert!(session.gate().is_none());

        pending["stage"] = json!("merge_confirmation");
        pending["pr"] = json!(73);
        fs::write(&state, serde_json::to_vec(&pending)?)?;
        session.refresh();
        assert!(session.confirm().is_err());
        session.acknowledged = session.gate();
        session.confirm()?;
        drop(session);
        let output = child.wait_with_output()?;
        assert!(output.status.success());
        let sha = "2".repeat(40);
        assert_eq!(
            String::from_utf8(output.stdout)?,
            format!("publish {sha}\nmerge 73 {sha}\n")
        );
        Ok(())
    }
}
