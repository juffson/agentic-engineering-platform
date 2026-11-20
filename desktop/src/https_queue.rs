use crate::{
    Runner,
    https_pilot::{Connection, Session},
};
use aep::{
    delivery::slug,
    workspace::{Workspace, git},
};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File},
    process::Command,
    time::Instant,
};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct Run {
    pub id: String,
    pub task: Ticket,
    pub stage: String,
    pub expires_at: Option<u64>,
    pub worker: Option<String>,
    #[serde(default)]
    events: Vec<Event>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct Ticket {
    pub ticket: String,
    pub name: String,
    pub target: String,
    repository: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
struct Event {
    progress: Phase,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
struct Phase {
    phase: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct Worker {
    #[serde(default)]
    pub online: bool,
}

impl Run {
    pub fn phase_label(&self) -> &str {
        match self.stage.as_str() {
            "queued" if self.worker.is_some() => "等待原 Worker",
            "queued" => "排队中",
            "cancel_pending" => "等待 Worker 确认停止",
            "interrupted" => "断线，等待恢复",
            _ => match self
                .events
                .last()
                .and_then(|event| event.progress.phase.as_deref())
            {
                Some("preparing") => "正在准备",
                Some("implementing") => "Codex 实现",
                Some("checking") => "运行验收",
                Some("reviewing") => "Claude 审查",
                _ => "运行中",
            },
        }
    }

    pub fn can_cancel(&self) -> bool {
        matches!(self.stage.as_str(), "queued" | "running")
            || (self.stage == "interrupted" && self.expires_at.is_some())
    }
}

#[derive(Deserialize)]
pub(crate) struct Queue {
    pub paused: bool,
    pub order: Vec<String>,
}

#[derive(Deserialize)]
pub(crate) struct Snapshot {
    pub queue: Option<Queue>,
    #[serde(default)]
    pub workers: BTreeMap<String, Worker>,
    pub attempts: Vec<Run>,
}

impl Snapshot {
    pub fn current(&self, worker: &str) -> Option<&Run> {
        self.attempts.iter().rev().find(|run| {
            run.worker.as_deref() == Some(worker)
                && (matches!(run.stage.as_str(), "running" | "cancel_pending")
                    || (run.stage == "interrupted" && run.expires_at.is_some()))
        })
    }

    pub fn visible_runs(&self) -> Vec<&Run> {
        let mut seen = HashSet::new();
        let mut rows = self
            .attempts
            .iter()
            .rev()
            .filter(|run| {
                seen.insert(&run.task.ticket) && (run.can_cancel() || run.stage == "cancel_pending")
            })
            .collect::<Vec<_>>();
        rows.sort_by_cached_key(|run| {
            if run.stage == "queued" {
                self.queue
                    .as_ref()
                    .and_then(|queue| queue.order.iter().position(|id| *id == run.id))
                    .unwrap_or(usize::MAX - 1)
                    + 1
            } else {
                0
            }
        });
        rows
    }
}

#[derive(Clone)]
pub(crate) enum Action {
    Status,
    Pause(bool),
    Move {
        run: String,
        adjacent: String,
        earlier: bool,
    },
    Cancel(Run),
}

pub(crate) struct Site {
    pub source: Workspace,
    pub connection: Connection,
    pub snapshot: Option<Snapshot>,
    pub pending: Option<Run>,
    pub busy: bool,
    pub checked: Option<Instant>,
    pub error: Option<String>,
    pub action_error: Option<String>,
}

impl Site {
    pub fn new(source: Workspace, connection: Connection) -> Self {
        Self {
            source,
            connection,
            snapshot: None,
            pending: None,
            busy: false,
            checked: None,
            error: None,
            action_error: None,
        }
    }

    pub fn restore(source: &Workspace) -> Option<Self> {
        let connection = File::open(source.data.join("https-studio/queue-site.json"))
            .ok()
            .and_then(|file| serde_json::from_reader(file).ok())
            .or_else(|| Session::saved_connection(source))?;
        Some(Self::new(source.clone(), connection))
    }

    pub fn save(&self) -> Result<()> {
        let parent = self.source.data.join("https-studio");
        fs::create_dir_all(&parent)?;
        let file = tempfile::NamedTempFile::new_in(&parent)?;
        serde_json::to_writer(file.as_file(), &self.connection)?;
        file.persist(parent.join("queue-site.json"))?;
        Ok(())
    }
}

pub(crate) fn perform(
    source: &Workspace,
    connection: &Connection,
    runner: &Runner,
    action: Action,
) -> Result<Snapshot> {
    let repository = slug(git(&source.root, &["remote", "get-url", "origin"])?.trim())?;
    let token = fs::read_to_string(&connection.operator_file).context("无法读取操作员令牌文件")?;
    let command = |argv: &[&str]| -> Result<Vec<u8>> {
        let mut process = Command::new(runner.binary()?);
        process
            .current_dir(&source.root)
            .env("PATH", runner.path()?)
            .env("AEP_CONTROL_OPERATOR_TOKEN", token.trim())
            .env_remove("AEP_CONTROL_DEVICE_TOKEN")
            .args(["control", "--url", &connection.endpoint]);
        if let Some(ca) = &connection.ca {
            process.arg("--ca-cert").arg(ca);
        }
        let result = process.args(argv).output()?;
        ensure!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr).trim()
        );
        Ok(result.stdout)
    };
    let read = || -> Result<Snapshot> {
        #[derive(Deserialize)]
        struct Reply {
            data: Snapshot,
        }
        let reply: Reply = serde_json::from_slice(&command(&["status"])?)
            .context("HTTPS 控制面返回了无效的队列状态")?;
        ensure!(
            reply
                .data
                .attempts
                .iter()
                .all(|run| run.task.repository == repository),
            "控制面属于另一个仓库"
        );
        Ok(reply.data)
    };
    let snapshot = read()?;
    match action {
        Action::Status => return Ok(snapshot),
        action => {
            ensure!(
                snapshot.queue.is_some(),
                "请先将 HTTPS 控制面升级到支持队列管理的版本"
            );
            let boolean = |yes| if yes { "true" } else { "false" };
            match action {
                Action::Pause(paused) => command(&["queue-pause", "--paused", boolean(paused)])?,
                Action::Move {
                    run,
                    adjacent,
                    earlier,
                } => command(&[
                    "queue-move",
                    "--run",
                    &run,
                    "--adjacent",
                    &adjacent,
                    "--earlier",
                    boolean(earlier),
                ])?,
                Action::Cancel(run) => command(&[
                    "cancel",
                    "--ticket",
                    &run.task.ticket,
                    "--run",
                    &run.id,
                    "--stage",
                    &run.stage,
                ])?,
                Action::Status => unreachable!(),
            };
        }
    }
    read()
}
