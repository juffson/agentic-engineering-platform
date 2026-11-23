use super::{Fixture, Plan, load_session};
use crate::relay::{self, Relay, Round};
use aep::{
    studio::Snapshot,
    workspace::{Workspace, read},
};
use anyhow::{Context, Result};
use std::{
    fs::{self, File, TryLockError},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    Preparing,
    PrepareRetry,
    Prepared,
    Dispatched,
    Start,
    Running(Round),
    Retry(Round),
    Decision,
    Continue,
    Review,
    PublishRetry,
    Checks,
    ChecksFailed,
    Merge,
    Cleanup,
    Archive,
    Archived,
    Unavailable,
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Preparing => "正在准备任务",
            Self::PrepareRetry => "准备失败或中断",
            Self::Prepared => "等待派发",
            Self::Dispatched => "已派发，等待第一轮",
            Self::Start => "等待第一轮",
            Self::Running(Round::First) => "第一轮运行中",
            Self::Running(Round::Second) => "第二轮运行中",
            Self::Retry(Round::First) => "第一轮需重试",
            Self::Retry(Round::Second) => "第二轮需重试",
            Self::Decision => "等待记忆修订",
            Self::Continue => "决策已保存，等待第二轮",
            Self::Review => "等待人工审查",
            Self::PublishRetry => "发布需重试",
            Self::Checks => "跟踪 GitHub CI",
            Self::ChecksFailed => "GitHub 检查失败",
            Self::Merge => "等待确认合并",
            Self::Cleanup => "已合并，等待收尾",
            Self::Archive => "等待证据归档",
            Self::Archived => "已归档",
            Self::Unavailable => "记录无法打开",
        }
    }
}

pub(crate) struct Entry {
    pub path: PathBuf,
    pub task: String,
    pub branch: String,
    pub stage: Stage,
    pub detail: Option<String>,
}

pub(crate) struct Progress {
    pub stage: Stage,
    pub detail: Option<String>,
    pub relay: Option<Relay>,
}

impl Progress {
    pub fn read(path: &Path, ws: &Workspace, fixture: &Fixture) -> Result<Self> {
        let out = path.join("evidence");
        let relay = relay::load(ws)?
            .into_iter()
            .find(|relay| relay.task.name == fixture.task && relay.task.sha == fixture.spec);
        let (stage, detail) = if out.join("verification.json").is_file() {
            if path.join("evidence.tar.gz").is_file() {
                (Stage::Archived, None)
            } else {
                (Stage::Archive, failure(&out, "archive-error.txt"))
            }
        } else if let Some(relay) = &relay {
            let snapshot = Snapshot::load(ws);
            let (stage, mut detail) = evaluate(relay, &snapshot, &out);
            if let Stage::Retry(round) = stage
                && let Some(reason) = relay
                    .run(round, &snapshot)
                    .and_then(|run| relay::implementation_failure(ws, run))
            {
                detail = Some(reason);
            }
            (stage, detail)
        } else if fixture.authored && out.join("dispatch.json").is_file() {
            (Stage::Dispatched, failure(&out, "dispatch-error.txt"))
        } else if fixture.authored {
            (Stage::Prepared, failure(&out, "dispatch-error.txt"))
        } else {
            (Stage::Start, None)
        };
        Ok(Self {
            stage,
            detail,
            relay,
        })
    }
}

pub(crate) fn load(ws: &Workspace) -> Result<Vec<Entry>> {
    let dir = ws.data.join("lifecycle");
    let files = match fs::read_dir(dir) {
        Ok(files) => files,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut sessions = files
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .collect::<Vec<_>>();
    sessions.sort_by_cached_key(|entry| {
        std::cmp::Reverse(
            entry
                .path()
                .join("evidence")
                .metadata()
                .and_then(|m| m.modified())
                .ok(),
        )
    });
    Ok(sessions
        .into_iter()
        .filter_map(|entry| entry_at(entry.path()))
        .collect())
}

fn entry_at(path: PathBuf) -> Option<Entry> {
    let out = path.join("evidence");
    let (task, branch, stage, detail) = if let Some(fixture) = load_session(&path) {
        let (stage, detail) = match Workspace::open(&path.join("workspace/repo"))
            .and_then(|ws| Progress::read(&path, &ws, &fixture))
        {
            Ok(value) => (value.stage, value.detail),
            Err(error) => (Stage::Unavailable, Some(format!("{error:#}"))),
        };
        (fixture.task, fixture.base, stage, detail)
    } else if let Ok(plan) = request(&path) {
        let (task, branch) = match plan {
            Plan::New(draft, branch) => (draft.name, branch.name),
            Plan::Existing(task, branch) => (task.name, branch.name),
            Plan::Synthetic => ("合成验证".into(), String::new()),
        };
        let busy = File::open(path.join("prepare.lock"))
            .ok()
            .is_some_and(|lock| matches!(lock.try_lock_shared(), Err(TryLockError::WouldBlock)));
        if busy {
            (task, branch, Stage::Preparing, None)
        } else {
            let detail = failure(&out, "failure.txt")
                .or_else(|| Some("Studio 在准备 Spec 时退出。可用原参数重试。".into()));
            (task, branch, Stage::PrepareRetry, detail)
        }
    } else {
        let detail = failure(&out, "failure.txt").or_else(|| {
            out.join("fixture.json")
                .is_file()
                .then(|| "无法读取任务凭据。".into())
        })?;
        let name = path.file_name()?.to_string_lossy().into_owned();
        (name, String::new(), Stage::Unavailable, Some(detail))
    };
    Some(Entry {
        path,
        task,
        branch,
        stage,
        detail,
    })
}

pub(crate) fn request(path: &Path) -> Result<Plan> {
    let file = File::open(path.join("request.json")).context("缺少任务准备参数")?;
    Ok(serde_json::from_reader(file)?)
}

fn failure(out: &Path, name: &str) -> Option<String> {
    let text = fs::read_to_string(out.join(name)).ok()?;
    (!text.trim().is_empty()).then(|| text.trim().to_owned())
}

fn evaluate(relay: &Relay, snapshot: &Snapshot, out: &Path) -> (Stage, Option<String>) {
    let round = if relay.latest(Round::Second).is_some() {
        Round::Second
    } else {
        Round::First
    };
    let run = relay.run(round, snapshot);
    let Some(attempt) = relay.latest(round) else {
        return (Stage::Start, None);
    };
    if relay.active(snapshot) {
        let phase = run.map(|run| relay::phase_label(run.text("phase")));
        return (Stage::Running(round), phase.map(str::to_owned));
    }
    if !relay::passed(run) {
        let error = run
            .and_then(|run| run.record["error"].as_str())
            .map(str::to_owned)
            .or_else(|| attempt.diagnostics.clone())
            .unwrap_or_else(|| "执行器已停止。重试会创建新的运行记录。".into());
        return (Stage::Retry(round), Some(error));
    }
    if round == Round::First {
        let stage = if relay.decisions(snapshot).is_empty() {
            Stage::Decision
        } else {
            Stage::Continue
        };
        return (stage, None);
    }
    if attempt.merged() || attempt.cleanup.is_some() {
        let reason = attempt.cleanup.as_ref().and_then(|receipt| {
            std::iter::once(&receipt.target)
                .chain(receipt.branch.iter())
                .chain(receipt.worktrees.iter().map(|entry| &entry.step))
                .find(|step| !step.done)
                .map(|step| step.detail.clone())
        });
        return (Stage::Cleanup, failure(out, "cleanup-error.txt").or(reason));
    }
    let Some(publication) = &attempt.publication else {
        let detail = failure(out, "publish-error.txt");
        return (
            if detail.is_some() {
                Stage::PublishRetry
            } else {
                Stage::Review
            },
            detail,
        );
    };
    let Some(pr) = &publication.pr else {
        return (
            Stage::PublishRetry,
            publication
                .error
                .clone()
                .or_else(|| Some("发布尚未完成，可在审查页确认后继续。".into())),
        );
    };
    let cached = read(&out.join("github-progress.json"))
        .ok()
        .filter(|value| {
            value["pr"] == pr.number && value["candidate"] == publication.target.candidate
        });
    let stage = match cached.as_ref().and_then(|item| item["state"].as_str()) {
        Some("merged") => Stage::Cleanup,
        Some("failed") => Stage::ChecksFailed,
        Some("ready") => Stage::Merge,
        _ => Stage::Checks,
    };
    let detail = [
        "merge-error.txt",
        "github-retry-error.txt",
        "github-refresh-error.txt",
    ]
    .into_iter()
    .find_map(|name| failure(out, name))
    .or_else(|| {
        cached
            .as_ref()
            .and_then(|item| item["detail"].as_str())
            .map(str::to_owned)
    });
    (stage, detail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        lifecycle::{Branch, Draft, save},
        relay::{Attempt, Task},
    };
    use aep::studio::Run;
    use aep::task::TaskManifest;
    use serde_json::json;

    fn attempt(round: Round, id: &str) -> Attempt {
        Attempt {
            id: id.into(),
            round,
            decision: None,
            diagnostics: None,
            publication: None,
            merge: None,
            cleanup: None,
        }
    }

    #[test]
    fn an_interrupted_preparation_is_recoverable_only_from_its_repository() -> Result<()> {
        let root = tempfile::tempdir()?;
        let ws = Workspace {
            root: root.path().into(),
            data: root.path().join("aep"),
        };
        let unrelated = Workspace {
            root: root.path().into(),
            data: root.path().join("elsewhere"),
        };
        let path = ws.data.join("lifecycle/studio-test");
        fs::create_dir_all(path.join("evidence"))?;
        let plan = Plan::New(
            Draft {
                name: "sample".into(),
                spec: "Spec is preserved".into(),
                manifest: TaskManifest {
                    title: None,
                    allowed_paths: vec!["src/lib.rs".into()],
                    check: vec!["cargo".into(), "test".into()],
                    criteria: vec![],
                    checks: vec![],
                },
            },
            Branch {
                name: "main".into(),
                sha: "pinned".into(),
            },
        );
        let lock = File::create(path.join("prepare.lock"))?;
        lock.try_lock()?;
        save(&path, "request.json", &plan)?;
        assert!(load(&unrelated)?.is_empty());
        assert_eq!(load(&ws)?[0].stage, Stage::Preparing);
        // Parallel tests can briefly inherit the forked descriptor until Git execs.
        lock.unlock()?;
        assert_eq!(load(&ws)?[0].stage, Stage::PrepareRetry);
        let Plan::New(draft, branch) = request(&path)? else {
            anyhow::bail!("draft lost")
        };
        assert_eq!(draft.spec, "Spec is preserved");
        assert_eq!(branch.sha, "pinned");
        Ok(())
    }

    #[test]
    fn retry_and_decision_are_derived_from_the_latest_attempt() -> Result<()> {
        let evidence = tempfile::tempdir()?;
        let mut relay = Relay {
            stage_plan: None,
            id: "relay-test".into(),
            task: Task {
                name: "test".into(),
                sha: "spec".into(),
            },
            agents: Default::default(),
            attempts: vec![attempt(Round::First, "first")],
            busy: true,
        };
        let mut snapshot = Snapshot {
            runs: vec![Run {
                id: "first".into(),
                record: json!({"status":"failed","error":"acceptance exited 1"}),
            }],
            ..Snapshot::default()
        };
        assert_eq!(
            evaluate(&relay, &snapshot, evidence.path()).0,
            Stage::Running(Round::First)
        );
        relay.busy = false;
        assert_eq!(
            evaluate(&relay, &snapshot, evidence.path()),
            (
                Stage::Retry(Round::First),
                Some("acceptance exited 1".into())
            )
        );
        relay.attempts.push(attempt(Round::First, "retry"));
        snapshot.runs.push(Run {
            id: "retry".into(),
            record: json!({"status":"awaiting_human_review","check_exit_code":0,"review_status":"findings_recorded"}),
        });
        assert_eq!(
            evaluate(&relay, &snapshot, evidence.path()).0,
            Stage::Decision
        );
        snapshot.notes.push(json!({"id":1,"kind":"decision","scope":"task","author":"manual","task":"test","spec_sha":"spec"}));
        assert_eq!(
            evaluate(&relay, &snapshot, evidence.path()).0,
            Stage::Continue
        );
        relay.attempts.push(attempt(Round::Second, "interrupted"));
        snapshot.runs.push(Run {
            id: "interrupted".into(),
            record: json!({"status":"needs_reconciliation"}),
        });
        assert_eq!(
            evaluate(&relay, &snapshot, evidence.path()).0,
            Stage::Retry(Round::Second)
        );
        Ok(())
    }
}
