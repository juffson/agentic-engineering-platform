use aep::workflow::Client;
use aep::{
    cancel::{Cancellation, Cancelled},
    studio::{Artifact, Run, Snapshot, preview},
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Round {
    First,
    Second,
}

pub(crate) fn phase_label(name: &str) -> &'static str {
    match name {
        "preparing" => "准备工作区",
        "implementing" => "实现中",
        "checking" => "运行验收",
        "reviewing" => "审查中",
        "complete" => "等待人工审查",
        _ => "启动执行器",
    }
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
    #[serde(skip)]
    pub merge: Option<crate::github_pr::MergeReceipt>,
    #[serde(skip)]
    pub cleanup: Option<crate::cleanup::Receipt>,
}
impl Attempt {
    pub fn merged(&self) -> bool {
        self.merge.as_ref().is_some_and(|merge| {
            merge.merged_sha.is_some()
                && self.publication.as_ref().is_some_and(|saved| {
                    saved.target.candidate == merge.head
                        && saved
                            .pr
                            .as_ref()
                            .is_some_and(|pr| pr.number == merge.number)
                })
        })
    }

    pub fn finalized(&self) -> bool {
        self.cleanup.as_ref().is_some_and(|receipt| {
            receipt.complete()
                && self.publication.as_ref().is_some_and(|saved| {
                    saved.target.candidate == receipt.candidate
                        && saved.pr.as_ref().is_some_and(|pr| pr.number == receipt.pr)
                })
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub(crate) struct RelayAgents {
    pub implementer: Client,
    pub reviewer: Client,
    // Keep the existing serialized Codex model field for older relay and queue records.
    pub model: String,
    pub zcode_model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewer_model: Option<String>,
}

impl RelayAgents {
    pub fn matches_evidence(&self, run: &serde_json::Value) -> bool {
        matches!(self.implementer, Client::Codex | Client::ZCode)
            && matches!(self.reviewer, Client::Claude | Client::ZCode)
            && run["implementation_source"] == self.implementer.binary()
            && run["reviewer"] == self.reviewer.binary()
    }
}

impl Default for RelayAgents {
    fn default() -> Self {
        Self {
            implementer: Client::Codex,
            reviewer: Client::Claude,
            model: String::new(),
            zcode_model: String::new(),
            reviewer_model: None,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct StagePlan {
    pub project: String,
    pub work: String,
    pub stage: usize,
    pub first: RelayAgents,
    pub second: RelayAgents,
}

pub(crate) struct StageSetup {
    pub plan: StagePlan,
    pub repo: PathBuf,
    pub title: String,
    pub tasks: Vec<Task>,
    pub editing_second: bool,
}
#[derive(Serialize, Deserialize)]
pub(crate) struct Relay {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage_plan: Option<StagePlan>,
    pub id: String,
    pub task: Task,
    #[serde(flatten)]
    pub agents: RelayAgents,
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
        let mut roots = self.questions(snapshot);
        let legacy_task_decision = roots.is_empty() && self.stage_plan.is_none();
        if roots.is_empty()
            && let Some(first) = self.latest(Round::First)
        {
            roots.extend(
                snapshot
                    .notes
                    .iter()
                    .filter(|n| n["author"] == "manual" && n["run_id"] == first.id),
            );
        }
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
                    && (legacy_task_decision
                        || roots.iter().any(|root| root["id"] == note["id"])
                        || revises_question(note, &roots, &snapshot.notes))
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

pub(crate) fn implementation_failure(ws: &Workspace, run: &Run) -> Option<String> {
    if run.text("phase") != "implementing" || run.text("status") != "failed" {
        return None;
    }
    let log = preview(&ws.data, &run.id, Artifact::Implementation).ok()??;
    for line in log.text.lines().rev() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let text = match event["type"].as_str() {
            Some("turn.failed") => event["error"]["message"].as_str(),
            Some("error") => event["message"].as_str(),
            _ => None,
        };
        if let Some(text) = text {
            let decoded = serde_json::from_str::<Value>(text).ok();
            let text = decoded
                .as_ref()
                .and_then(|value| value["error"]["message"].as_str())
                .unwrap_or(text);
            let text = text.trim();
            if !text.is_empty() {
                return Some(text.chars().take(500).collect());
            }
        }
    }
    None
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
    Ok(tasks_from_heads(&output))
}

/// Stage relays run against immutable Spec commits already present in this checkout.
pub(crate) fn stage_tasks(ws: &Workspace) -> Result<Vec<Task>> {
    let head = commit(&ws.root, "HEAD")?;
    let mut tasks = std::collections::BTreeMap::new();
    for file in git_quiet(ws, &["ls-tree", "-r", "--name-only", &head, "specs/"])?.lines() {
        if let Some(name) = file
            .strip_prefix("specs/")
            .and_then(|s| s.strip_suffix("/task.json"))
            && ident(name, 63).is_ok()
        {
            tasks.insert(
                name.to_owned(),
                Task {
                    name: name.to_owned(),
                    sha: head.clone(),
                },
            );
        }
    }
    for task in tasks_from_heads(&git_quiet(
        ws,
        &[
            "for-each-ref",
            "--format=%(objectname)%09%(refname)",
            "refs/heads/aep-task/",
        ],
    )?) {
        tasks.insert(task.name.clone(), task);
    }
    Ok(tasks.into_values().collect())
}

pub(crate) fn tasks_from_heads(output: &str) -> Vec<Task> {
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
    tasks
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
                    attempt.merge = crate::github_pr::read_receipt(&run)?;
                    attempt.cleanup = crate::cleanup::read(&run)?;
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

pub(crate) fn new_id() -> String {
    format!("relay-{}", uuid::Uuid::new_v4().simple())
}

pub(crate) struct Lease {
    id: String,
    path: PathBuf,
    owner: File,
}

impl Lease {
    pub fn acquire(ws: &Workspace, id: &str) -> Result<Option<Self>> {
        let path = folder(ws, id)?;
        fs::create_dir_all(&path)?;
        let owner = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path.join("owner.lock"))?;
        match owner.try_lock() {
            Ok(()) => Ok(Some(Self {
                id: id.into(),
                path,
                owner,
            })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => Err(error.into()),
        }
    }

    fn read(&self) -> Result<Option<Relay>> {
        match File::open(self.path.join("relay.json")) {
            Ok(file) => {
                let relay: Relay = serde_json::from_reader(file)?;
                ensure!(relay.id == self.id, "接力 ID 与持久记录不一致");
                Ok(Some(relay))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub fn planned_run(&self, round: Round) -> Result<String> {
        let count = self.read()?.map_or(0, |relay| relay.attempts.len());
        Ok(attempt_id(&self.id, round, count))
    }

    pub fn has_attempt(&self, id: &str) -> Result<bool> {
        Ok(self
            .read()?
            .is_some_and(|relay| relay.attempts.iter().any(|attempt| attempt.id == id)))
    }

    pub fn start(
        &self,
        ws: &Workspace,
        runner: &Runner,
        task: Task,
        agents: RelayAgents,
    ) -> Result<()> {
        ensure!(self.read()?.is_none(), "此接力已存在");
        let mut relay = Relay {
            stage_plan: None,
            id: self.id.clone(),
            task,
            agents,
            attempts: Vec::new(),
            busy: false,
        };
        launch(
            ws,
            runner,
            &self.owner,
            &mut relay,
            Round::First,
            None,
            None,
        )
    }

    /// Save the plan before linking it. A reopened item can start its first attempt.
    pub fn prepare_stage(
        &self,
        task: Task,
        plan: StagePlan,
        link: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        ensure!(self.read()?.is_none(), "此接力已存在");
        save(
            &self.path,
            &Relay {
                agents: plan.first.clone(),
                stage_plan: Some(plan),
                id: self.id.clone(),
                task,
                attempts: Vec::new(),
                busy: false,
            },
        )?;
        if let Err(e) = link() {
            fs::remove_file(self.path.join("relay.json")).context("清理未关联的空接力")?;
            return Err(e);
        }
        Ok(())
    }

    pub fn advance(
        &self,
        ws: &Workspace,
        runner: &Runner,
        round: Round,
        decision: Option<i64>,
        agents: RelayAgents,
    ) -> Result<()> {
        let mut relay = self.read()?.context("找不到接力记录")?;
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
        if let Some(plan) = &mut relay.stage_plan {
            match round {
                Round::First => plan.first = agents.clone(),
                Round::Second => plan.second = agents.clone(),
            }
        }
        relay.agents = agents;
        launch(
            ws,
            runner,
            &self.owner,
            &mut relay,
            round,
            decision,
            resume.as_deref(),
        )
    }
}

pub(crate) fn start(
    ws: &Workspace,
    runner: &Runner,
    id: String,
    task: Task,
    agents: RelayAgents,
) -> Result<()> {
    Lease::acquire(ws, &id)?
        .context("此接力正在执行")?
        .start(ws, runner, task, agents)
}

pub(crate) fn advance(
    ws: &Workspace,
    runner: &Runner,
    id: &str,
    round: Round,
    decision: Option<i64>,
    agents: RelayAgents,
) -> Result<()> {
    Lease::acquire(ws, id)?
        .context("此接力正在执行")?
        .advance(ws, runner, round, decision, agents)
}

fn attempt_id(relay: &str, round: Round, completed: usize) -> String {
    format!("{}-{}-{}", relay, round.suffix(), completed + 1)
}

fn launch(
    ws: &Workspace,
    runner: &Runner,
    owner: &File,
    relay: &mut Relay,
    round: Round,
    decision: Option<i64>,
    resume: Option<&str>,
) -> Result<()> {
    let path = folder(ws, &relay.id)?;
    let run_id = attempt_id(&relay.id, round, relay.attempts.len());
    let cancellation = Cancellation::new(&ws.data, &run_id)?;
    cancellation.check()?;
    relay.attempts.push(Attempt {
        id: run_id.clone(),
        round,
        decision,
        diagnostics: None,
        publication: None,
        merge: None,
        cleanup: None,
    });
    save(&path, relay)?;
    let mut log = File::create(path.join(format!("{run_id}.log")))?;
    let result = (|| {
        if round == Round::First {
            ident(&relay.task.name, 63)?;
            let spec = format!("refs/heads/aep-task/{}", relay.task.name);
            let pinned = format!("refs/aep/relays/{}/spec", relay.id);
            if relay.stage_plan.is_some() {
                git_quiet(ws, &["update-ref", &pinned, &relay.task.sha])?;
            } else {
                git_quiet(
                    ws,
                    &["fetch", "--no-tags", "origin", &format!("{spec}:{pinned}")],
                )?;
            }
            ensure!(
                commit(&ws.root, &pinned)? == relay.task.sha,
                "任务的派发版本已变化，请刷新任务列表"
            );
        }
        cancellation.check()?;
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
                relay.agents.implementer.binary(),
                "--reviewer",
                relay.agents.reviewer.binary(),
                "--shared-memory",
                "--run-id",
                &run_id,
            ])
            .env("PATH", runner.path()?)
            .env("GIT_TERMINAL_PROMPT", "0")
            .process_group(0)
            // AEP does not read stdin. Keep the relay lease in the child if Studio exits.
            .stdin(Stdio::from(owner.try_clone()?))
            .stdout(log.try_clone()?)
            .stderr(log.try_clone()?);
        if relay.agents.implementer == Client::Codex && !relay.agents.model.is_empty() {
            command.args(["--codex-model", &relay.agents.model]);
        }
        if (relay.agents.implementer == Client::ZCode
            || (relay.agents.reviewer == Client::ZCode && relay.agents.reviewer_model.is_none()))
            && !relay.agents.zcode_model.is_empty()
        {
            command.args(["--zcode-model", &relay.agents.zcode_model]);
        }
        if let Some(model) = &relay.agents.reviewer_model {
            command.arg(format!("--reviewer-model={model}"));
        }
        if let Some(resume) = resume {
            command.args(["--resume-from", resume]);
            let first = relay.latest(Round::First).context("缺少第一轮交接")?;
            command.args(["--handoff-from-run", &first.id]);
        }
        if let Some(decision) = decision {
            command.args(["--require-memory", &decision.to_string()]);
        }
        let exit = command.status().context("无法启动 AEP 执行器")?;
        if !exit.success() {
            let evidence = status(&ws.data, &run_id).unwrap_or_default();
            if evidence["status"] == "cancelled" {
                return Err(Cancelled.into());
            }
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

    pub(crate) fn binary(&self) -> Result<PathBuf> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    #[test]
    fn old_and_mixed_relay_profiles_round_trip() -> Result<()> {
        let legacy =
            serde_json::json!({"id":"r","task":{"name":"t","sha":"s"},"model":"old","attempts":[]});
        let mut relay: Relay = serde_json::from_value(legacy)?;
        assert_eq!(relay.agents.model, "old");
        assert_eq!(relay.agents.implementer, Client::Codex);
        assert_eq!(relay.agents.reviewer, Client::Claude);
        relay.agents.reviewer = Client::ZCode;
        relay.agents.zcode_model = "team/model@high".into();
        let restored: Relay = serde_json::from_slice(&serde_json::to_vec(&relay)?)?;
        assert_eq!(restored.agents, relay.agents);
        assert!(restored.agents.matches_evidence(
            &serde_json::json!({"implementation_source":"codex","reviewer":"zcode"})
        ));
        assert!(!restored.agents.matches_evidence(
            &serde_json::json!({"implementation_source":"codex","reviewer":"claude"})
        ));
        Ok(())
    }

    #[test]
    fn stage_plan_reopens_and_pins_a_local_spec_without_origin() -> Result<()> {
        use aep::workspace::git;
        let dir = tempfile::tempdir()?;
        let repo = dir.path();
        git(repo, &["init", "-q"])?;
        let files = repo.join("specs/sample");
        fs::create_dir_all(&files)?;
        fs::write(files.join("task.json"), "{}")?;
        git(repo, &["add", "specs"])?;
        git(
            repo,
            &[
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-qm",
                "sample",
            ],
        )?;
        let ws = Workspace::open(repo)?;
        let task = stage_tasks(&ws)?.pop().context("local Spec")?;
        let plan = StagePlan {
            project: "p".into(),
            work: "w".into(),
            stage: 3,
            first: RelayAgents::default(),
            second: RelayAgents {
                reviewer: Client::ZCode,
                reviewer_model: Some("vendor/test".into()),
                ..Default::default()
            },
        };
        let lease = Lease::acquire(&ws, "relay-stage")?.unwrap();
        assert!(
            lease
                .prepare_stage(task.clone(), plan.clone(), || anyhow::bail!(
                    "stale catalog"
                ))
                .is_err()
        );
        assert!(load(&ws)?.is_empty());
        lease.prepare_stage(task.clone(), plan.clone(), || Ok(()))?;
        drop(lease);
        let saved = load_one(&ws, "relay-stage")?;
        assert!(saved.attempts.is_empty());
        assert_eq!(saved.stage_plan.unwrap().second, plan.second);
        let runner = Runner {
            binary: Some("/usr/bin/true".into()),
            ..Default::default()
        };
        let lease = Lease::acquire(&ws, "relay-stage")?.unwrap();
        lease.advance(&ws, &runner, Round::First, None, plan.first)?;
        assert_eq!(commit(repo, "refs/aep/relays/relay-stage/spec")?, task.sha);
        assert!(lease.has_attempt("relay-stage-r1-1")?);
        Ok(())
    }

    #[test]
    fn stage_decision_without_question_stays_with_its_original_relay() -> Result<()> {
        use serde_json::json;
        let mut relay: Relay = serde_json::from_value(json!({
            "id":"r", "task":{"name":"t","sha":"s"},
            "stage_plan":{"project":"p","work":"w","stage":1,"first":{},"second":{}},
            "attempts":[{"id":"first","round":"First","decision":null}]
        }))?;
        let decision = |id, run, replaces, successor| {
            json!({
                "id":id, "run_id":run, "replaces":replaces, "successor":successor, "author":"manual",
                "task":"t", "spec_sha":"s", "scope":"task", "kind":"decision"
            })
        };
        let snapshot = Snapshot {
            notes: vec![
                decision(1, "other", None, None),
                decision(2, "first", None, Some(3)),
                decision(3, "", Some(2), None),
            ],
            ..Default::default()
        };
        assert_eq!(relay.decisions(&snapshot), vec![&snapshot.notes[2]]);
        relay.stage_plan = None;
        assert_eq!(relay.decisions(&snapshot).len(), 2);
        Ok(())
    }

    #[test]
    fn executor_holds_the_relay_lease_after_studio_drops_it() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let ws = Workspace {
            root: dir.path().into(),
            data: dir.path().into(),
        };
        let lease = Lease::acquire(&ws, "test")?.context("initial lease")?;
        let child = Command::new("sh")
            .args([
                "-c",
                "echo ready; while [ ! -f finish ]; do sleep 0.01; done",
            ])
            .current_dir(dir.path())
            .stdin(Stdio::from(lease.owner.try_clone()?))
            .stdout(Stdio::piped())
            .spawn()?;
        let mut child = child;
        let mut ready = String::new();
        BufReader::new(child.stdout.take().context("child stdout")?).read_line(&mut ready)?;
        drop(lease);
        let held = Lease::acquire(&ws, "test")?.is_none();
        File::create(dir.path().join("finish"))?;
        assert!(child.wait()?.success());
        assert_eq!(ready.trim(), "ready");
        assert!(held, "the child released the relay lease too early");
        assert!(Lease::acquire(&ws, "test")?.is_some());
        Ok(())
    }
}
