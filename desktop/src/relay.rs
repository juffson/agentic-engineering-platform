use aep::{
    studio::{Run, Snapshot},
    workspace::{Workspace, commit, ident, status},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    env, fs,
    fs::{File, OpenOptions, TryLockError},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Clone, Default)]
pub struct Runner {
    pub binary: Option<PathBuf>,
    pub client_bin: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Task {
    pub name: String,
    pub sha: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Round {
    First,
    Second,
}
impl Round {
    fn suffix(self) -> &'static str {
        match self {
            Self::First => "r1",
            Self::Second => "r2",
        }
    }
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Attempt {
    pub id: String,
    pub round: Round,
    pub decision: Option<i64>,
    #[serde(skip)]
    pub diagnostics: Option<String>,
    #[serde(skip)]
    pub publication: Option<crate::publish::Publication>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Relay {
    pub id: String,
    pub task: Task,
    pub model: String,
    pub attempts: Vec<Attempt>,
    #[serde(skip)]
    pub busy: bool,
}
impl Relay {
    pub fn latest(&self, round: Round) -> Option<&Attempt> {
        self.attempts
            .iter()
            .rev()
            .find(|attempt| attempt.round == round)
    }

    pub fn run<'a>(&self, round: Round, snapshot: &'a Snapshot) -> Option<&'a Run> {
        let id = &self.latest(round)?.id;
        snapshot.runs.iter().find(|run| &run.id == id)
    }

    pub fn active(&self, snapshot: &Snapshot) -> bool {
        self.busy
            || [Round::First, Round::Second].into_iter().any(|round| {
                self.run(round, snapshot)
                    .is_some_and(|run| matches!(run.text("status"), "running" | "initializing"))
            })
    }

    pub fn questions<'a>(&self, snapshot: &'a Snapshot) -> Vec<&'a Value> {
        let Some(run) = self.run(Round::First, snapshot) else {
            return Vec::new();
        };
        snapshot
            .notes
            .iter()
            .filter(|note| {
                note["run_id"] == run.id
                    && note["scope"] == "task"
                    && note["kind"] == "open_question"
            })
            .collect()
    }

    pub fn decisions<'a>(&self, snapshot: &'a Snapshot) -> Vec<&'a Value> {
        let questions = self.questions(snapshot);
        snapshot
            .notes
            .iter()
            .filter(|note| {
                note["kind"] == "decision"
                    && note["scope"] == "task"
                    && note["author"] == "manual"
                    && note["task"] == self.task.name
                    && note["spec_sha"] == self.task.sha
                    && note["successor"].is_null()
                    && (questions.is_empty() || revises_question(note, &questions, &snapshot.notes))
            })
            .collect()
    }
}

fn revises_question<'a>(mut note: &'a Value, questions: &[&Value], notes: &'a [Value]) -> bool {
    while let (Some(id), Some(previous)) = (note["id"].as_i64(), note["replaces"].as_i64()) {
        if previous >= id {
            return false;
        }
        if questions.iter().any(|question| question["id"] == previous) {
            return true;
        }
        let Some(parent) = notes.iter().find(|entry| entry["id"] == previous) else {
            return false;
        };
        note = parent;
    }
    false
}

pub(crate) fn passed(run: Option<&Run>) -> bool {
    run.is_some_and(|run| {
        run.text("status") == "awaiting_human_review"
            && run.record["check_exit_code"] == 0
            && run.text("review_status") == "findings_recorded"
    })
}

fn git_quiet(ws: &Workspace, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(&ws.root)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .context("启动 Git")?;
    ensure!(
        output.status.success(),
        "Git {}: {}",
        args[0],
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(String::from_utf8(output.stdout)?)
}

pub(crate) fn tasks(ws: &Workspace) -> Result<Vec<Task>> {
    let output = git_quiet(
        ws,
        &["ls-remote", "--refs", "origin", "refs/heads/aep-task/*"],
    )?;
    let mut tasks: Vec<_> = output
        .lines()
        .filter_map(|line| {
            let (sha, name) = line.split_once('\t')?;
            let name = name.strip_prefix("refs/heads/aep-task/")?;
            ident(name, 63).ok()?;
            Some(Task {
                name: name.to_owned(),
                sha: sha.to_owned(),
            })
        })
        .collect();
    tasks.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(tasks)
}

fn folder(ws: &Workspace, id: &str) -> Result<PathBuf> {
    ident(id, 64)?;
    Ok(ws.data.join("relays").join(id))
}

pub(crate) fn load_one(ws: &Workspace, id: &str) -> Result<Relay> {
    read_relay(ws, &folder(ws, id)?)
}

fn read_relay(ws: &Workspace, path: &Path) -> Result<Relay> {
    let file = File::open(path.join("relay.json"))?;
    let mut relay: Relay = serde_json::from_reader(file)?;
    let lock = File::open(path.join("owner.lock"))?;
    relay.busy = match lock.try_lock_shared() {
        Ok(()) => false,
        Err(TryLockError::WouldBlock) => true,
        Err(TryLockError::Error(error)) => return Err(error.into()),
    };
    if !relay.busy {
        for attempt in &mut relay.attempts {
            ident(&attempt.id, 64)?;
            let run = ws.data.join("runs").join(&attempt.id);
            if run.join("evidence.json").exists() {
                if attempt.round == Round::Second {
                    attempt.publication = crate::publish::load(&run)?;
                }
                continue;
            }
            let Ok(mut file) = File::open(path.join(format!("{}.log", attempt.id))) else {
                continue;
            };
            let end = file.metadata()?.len();
            file.seek(SeekFrom::Start(end.saturating_sub(4096)))?;
            let mut buf = Vec::new();
            file.take(4096).read_to_end(&mut buf)?;
            let diagnostics = String::from_utf8_lossy(&buf).trim().to_owned();
            if !diagnostics.is_empty() {
                attempt.diagnostics = Some(diagnostics);
            }
        }
    }
    Ok(relay)
}

pub(crate) fn load(ws: &Workspace) -> Result<Vec<Relay>> {
    let path = ws.data.join("relays");
    if !path.try_exists()? {
        return Ok(Vec::new());
    }
    let mut entries = fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.metadata().and_then(|m| m.created()).ok()));
    entries
        .into_iter()
        .filter_map(|entry| {
            let path = entry.path();
            path.join("relay.json")
                .exists()
                .then(|| read_relay(ws, &path))
        })
        .collect()
}

fn save(path: &Path, relay: &Relay) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(path)?;
    serde_json::to_writer_pretty(&mut file, relay)?;
    file.persist(path.join("relay.json"))?;
    Ok(())
}

fn lock(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.join("owner.lock"))?;
    file.try_lock().context("此接力正在执行")?;
    Ok(file)
}

pub(crate) fn new_id() -> String {
    format!("relay-{}", uuid::Uuid::new_v4().simple())
}

pub(crate) fn start(
    ws: &Workspace,
    runner: &Runner,
    id: String,
    task: Task,
    model: String,
) -> Result<()> {
    let path = folder(ws, &id)?;
    fs::create_dir_all(&path)?;
    let _lock = lock(&path)?;
    ensure!(!path.join("relay.json").exists(), "此接力已存在");
    let mut relay = Relay {
        id,
        task,
        model,
        attempts: Vec::new(),
        busy: false,
    };
    launch(ws, runner, &path, &mut relay, Round::First, None, None)
}

pub(crate) fn advance(
    ws: &Workspace,
    runner: &Runner,
    id: &str,
    round: Round,
    decision: Option<i64>,
    model: String,
) -> Result<()> {
    let path = folder(ws, id)?;
    let _lock = lock(&path)?;
    let mut relay: Relay = serde_json::from_reader(File::open(path.join("relay.json"))?)?;
    let snapshot = Snapshot::load(ws);
    ensure!(!relay.active(&snapshot), "此接力正在执行");
    ensure!(!passed(relay.run(round, &snapshot)), "此轮已完成");
    let resume = if round == Round::Second {
        let first = relay.run(Round::First, &snapshot);
        ensure!(passed(first), "第一轮尚未通过");
        let decision = decision.context("请选择任务决策")?;
        ensure!(
            relay
                .decisions(&snapshot)
                .iter()
                .any(|note| note["id"] == decision),
            "请选择当前任务的有效决策"
        );
        Some(
            first
                .context("缺少第一轮证据")?
                .text("candidate_sha")
                .to_owned(),
        )
    } else {
        None
    };
    relay.model = model;
    launch(
        ws,
        runner,
        &path,
        &mut relay,
        round,
        decision,
        resume.as_deref(),
    )
}

fn launch(
    ws: &Workspace,
    runner: &Runner,
    path: &Path,
    relay: &mut Relay,
    round: Round,
    decision: Option<i64>,
    resume: Option<&str>,
) -> Result<()> {
    let run_id = format!(
        "{}-{}-{}",
        relay.id,
        round.suffix(),
        relay.attempts.len() + 1
    );
    relay.attempts.push(Attempt {
        id: run_id.clone(),
        round,
        decision,
        diagnostics: None,
        publication: None,
    });
    save(path, relay)?;
    let mut log = File::create(path.join(format!("{run_id}.log")))?;
    let result = (|| {
        if round == Round::First {
            ident(&relay.task.name, 63)?;
            let spec = format!("refs/heads/aep-task/{}", relay.task.name);
            let pinned = format!("refs/aep/relays/{}/spec", relay.id);
            git_quiet(
                ws,
                &["fetch", "--no-tags", "origin", &format!("{spec}:{pinned}")],
            )?;
            ensure!(
                commit(&ws.root, &pinned)? == relay.task.sha,
                "任务的派发版本已变化，请刷新任务列表"
            );
        }
        let program = runner.binary()?;
        let mut command = Command::new(program);
        command
            .args(["--repo"])
            .arg(&ws.root)
            .args([
                "run",
                "--task",
                &relay.task.name,
                "--base",
                &relay.task.sha,
                "--implementer",
                "codex",
                "--reviewer",
                "claude",
                "--shared-memory",
                "--run-id",
                &run_id,
            ])
            .env("PATH", runner.path()?)
            .env("GIT_TERMINAL_PROMPT", "0")
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log.try_clone()?);
        if !relay.model.is_empty() {
            command.args(["--codex-model", &relay.model]);
        }
        if let Some(resume) = resume {
            command.args(["--resume-from", resume]);
        }
        if let Some(decision) = decision {
            command.args(["--require-memory", &decision.to_string()]);
        }
        let exit = command.status().context("无法启动 AEP 执行器")?;
        if !exit.success() {
            let evidence = status(&ws.data, &run_id).unwrap_or_default();
            anyhow::bail!(
                "{}",
                evidence["error"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("AEP 执行器退出 ({exit})"))
            );
        }
        Ok(())
    })();
    if let Err(error) = &result {
        writeln!(log, "{error:#}")?;
    }
    result
}

impl Runner {
    pub(crate) fn path(&self) -> Result<std::ffi::OsString> {
        let original = env::var_os("PATH").unwrap_or_default();
        let home = env::home_dir()
            .into_iter()
            .flat_map(|home| [home.join(".local/bin"), home.join(".cargo/bin")]);
        Ok(env::join_paths(
            self.client_bin
                .iter()
                .cloned()
                .chain(env::split_paths(&original))
                .chain(home)
                .chain(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from)),
        )?)
    }

    fn binary(&self) -> Result<PathBuf> {
        if let Some(binary) = &self.binary {
            return Ok(binary.clone());
        }
        if let Some(binary) = env::var_os("AEP_STUDIO_RUNNER") {
            return Ok(PathBuf::from(binary));
        }
        let sibling = env::current_exe()?.with_file_name("aep");
        if sibling.is_file() {
            return Ok(sibling);
        }
        env::split_paths(&self.path()?)
            .map(|path| path.join("aep"))
            .find(|path| path.is_file())
            .context("找不到 AEP 执行器。请使用打包版 Studio，或设置 AEP_STUDIO_RUNNER")
    }
}
