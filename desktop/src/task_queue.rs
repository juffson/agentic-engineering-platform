use crate::{
    Runner, lifecycle,
    relay::{Lease, Round, Task},
};
use aep::workspace::{Workspace, status};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    path::Path,
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

const FILE: &str = "task-queue.json";
pub(crate) const MAX_PARALLEL: usize = 4;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) enum Action {
    Start,
    RetryFirst,
    Second { decision: i64 },
}

impl Action {
    pub fn round(self) -> Round {
        match self {
            Self::Start | Self::RetryFirst => Round::First,
            Self::Second { .. } => Round::Second,
        }
    }

    pub fn label(self) -> &'static str {
        match self.round() {
            Round::First => "第一轮",
            Round::Second => "第二轮",
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) enum State {
    Waiting,
    Running,
    Passed,
    Failed(String),
}

impl State {
    pub fn active(&self) -> bool {
        matches!(self, Self::Waiting | Self::Running)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct Job {
    id: uuid::Uuid,
    pub session: String,
    task: String,
    spec: String,
    pub relay: String,
    pub action: Action,
    model: String,
    pub state: State,
    attempt: Option<String>,
    enqueued_at_ms: u128,
    started_at_ms: Option<u128>,
    finished_at_ms: Option<u128>,
}

impl Job {
    fn finish(&mut self, result: Result<()>) {
        self.state = match result {
            Ok(()) => State::Passed,
            Err(error) => State::Failed(format!("{error:#}")),
        };
        self.finished_at_ms = Some(now());
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct Queue {
    pub limit: usize,
    jobs: Vec<Job>,
}

impl Default for Queue {
    fn default() -> Self {
        Self {
            limit: 1,
            jobs: Vec::new(),
        }
    }
}

impl Queue {
    pub fn read(source: &Workspace) -> Result<Self> {
        Self::read_path(&source.data)
    }

    fn read_path(data: &Path) -> Result<Self> {
        let queue: Self = match File::open(data.join(FILE)) {
            Ok(file) => serde_json::from_reader(file).context("读取任务队列")?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => return Err(error.into()),
        };
        ensure!(
            (1..=MAX_PARALLEL).contains(&queue.limit),
            "任务队列的并发数无效"
        );
        Ok(queue)
    }

    pub fn latest(&self, session: &Path) -> Option<&Job> {
        let session = session.file_name()?.to_str()?;
        self.jobs.iter().rev().find(|job| job.session == session)
    }

    pub fn pending(&self) -> usize {
        self.jobs
            .iter()
            .filter(|job| job.state == State::Waiting)
            .count()
    }

    pub fn running(&self) -> usize {
        self.jobs
            .iter()
            .filter(|job| job.state == State::Running)
            .count()
    }

    pub fn position(&self, job: &Job) -> Option<usize> {
        self.jobs
            .iter()
            .filter(|entry| entry.state == State::Waiting)
            .position(|entry| entry.id == job.id)
            .map(|index| index + 1)
    }
}

fn now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn lock(source: &Workspace) -> Result<File> {
    fs::create_dir_all(&source.data)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(source.data.join("task-queue.lock"))?;
    file.lock()?;
    Ok(file)
}

fn save(source: &Workspace, queue: &Queue) -> Result<()> {
    lifecycle::save(&source.data, FILE, queue)
}

pub(crate) fn set_limit(source: &Workspace, limit: usize) -> Result<()> {
    ensure!((1..=MAX_PARALLEL).contains(&limit), "请选择有效的并发数");
    let _lock = lock(source)?;
    let mut queue = Queue::read(source)?;
    queue.limit = limit;
    save(source, &queue)
}

pub(crate) fn enqueue(
    source: &Workspace,
    ws: &Workspace,
    fixture: &lifecycle::Fixture,
    relay: String,
    action: Action,
    model: String,
) -> Result<()> {
    let root = lifecycle::root(ws).context("找不到任务工作区")?;
    let sessions = source.data.join("lifecycle");
    ensure!(
        fixture.real_task && root.parent() == Some(sessions.as_path()),
        "任务不属于当前仓库"
    );
    let session = root
        .file_name()
        .and_then(|name| name.to_str())
        .context("任务目录无效")?;
    let _lock = lock(source)?;
    let mut queue = Queue::read(source)?;
    ensure!(
        !queue
            .jobs
            .iter()
            .any(|job| job.session == session && job.state.active()),
        "此任务已在队列中或正在运行"
    );
    queue.jobs.push(Job {
        id: uuid::Uuid::new_v4(),
        session: session.into(),
        task: fixture.task.clone(),
        spec: fixture.spec.clone(),
        relay,
        action,
        model,
        state: State::Waiting,
        attempt: None,
        enqueued_at_ms: now(),
        started_at_ms: None,
        finished_at_ms: None,
    });
    save(source, &queue)
}

struct Checkout {
    ws: Workspace,
    fixture: lifecycle::Fixture,
    lease: Lease,
}

fn claim(source: &Workspace, job: &Job) -> Result<Option<Checkout>> {
    ensure!(
        job.session.starts_with("studio-")
            && job.session.len() > "studio-".len()
            && job
                .session
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
        "无效的队列任务目录"
    );
    let root = source.data.join("lifecycle").join(&job.session);
    ensure!(fs::symlink_metadata(&root)?.is_dir(), "任务目录已变化");
    let fixture = lifecycle::load_session(&root).context("找不到已保存的任务")?;
    ensure!(
        fixture.real_task && fixture.spec == job.spec && fixture.task == job.task,
        "排队的任务或 Spec 已变化"
    );
    let ws = Workspace::open(&root.join("workspace/repo"))?;
    ensure!(
        lifecycle::root(&ws) == Some(root.as_path()),
        "任务工作区已变化"
    );
    let Some(lease) = Lease::acquire(&ws, &job.relay)? else {
        return Ok(None);
    };
    Ok(Some(Checkout { ws, fixture, lease }))
}

fn reconcile(job: &mut Job, checkout: &Checkout) -> Result<()> {
    let attempt = job.attempt.as_deref().context("排队任务缺少运行 ID")?;
    if !checkout.lease.has_attempt(attempt)? {
        // Studio exited before the relay recorded a run. Dispatch can reuse the pinned Spec.
        job.state = State::Waiting;
        job.started_at_ms = None;
        job.attempt = None;
        return Ok(());
    }
    let record = status(&checkout.ws.data, attempt).unwrap_or_default();
    let result = if record["status"] == "awaiting_human_review"
        && record["check_exit_code"] == 0
        && record["review_status"] == "findings_recorded"
    {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "{}",
            record["error"]
                .as_str()
                .unwrap_or("执行器已中断，可重试本轮")
        ))
    };
    job.finish(result);
    Ok(())
}

fn run(checkout: &Checkout, runner: &Runner, job: &Job) -> Result<()> {
    let Checkout { ws, fixture, lease } = checkout;
    match job.action {
        Action::Start => {
            if fixture.authored {
                fixture.dispatch_authored(ws, runner)?;
            }
            lease.start(
                ws,
                runner,
                Task {
                    name: job.task.clone(),
                    sha: job.spec.clone(),
                },
                job.model.clone(),
            )
        }
        Action::RetryFirst => lease.advance(ws, runner, Round::First, None, job.model.clone()),
        Action::Second { decision } => {
            lease.advance(ws, runner, Round::Second, Some(decision), job.model.clone())
        }
    }
}

fn finish(source: &Workspace, id: uuid::Uuid, result: Result<()>) -> Result<()> {
    let _lock = lock(source)?;
    let mut queue = Queue::read(source)?;
    if let Some(job) = queue
        .jobs
        .iter_mut()
        .find(|job| job.id == id && job.state == State::Running)
    {
        job.finish(result);
        save(source, &queue)?;
    }
    Ok(())
}

fn reconcile_running(source: &Workspace, queue: &mut Queue) -> bool {
    let mut changed = false;
    for job in queue
        .jobs
        .iter_mut()
        .filter(|job| job.state == State::Running)
    {
        let result = match claim(source, job) {
            Ok(Some(checkout)) => reconcile(job, &checkout),
            Ok(None) => continue,
            Err(error) => Err(error),
        };
        if result.is_err() {
            job.finish(result);
        }
        changed = true;
    }
    changed
}

fn claim_waiting(source: &Workspace, queue: &mut Queue) -> (bool, Vec<(Job, Checkout)>) {
    let free = queue.limit.saturating_sub(queue.running());
    let mut ready = Vec::new();
    let mut changed = false;
    for job in queue
        .jobs
        .iter_mut()
        .filter(|job| job.state == State::Waiting)
    {
        if ready.len() == free {
            break;
        }
        let result = claim(source, job).and_then(|checkout| {
            checkout
                .map(|checkout| {
                    let attempt = checkout.lease.planned_run(job.action.round())?;
                    Ok::<_, anyhow::Error>((checkout, attempt))
                })
                .transpose()
        });
        match result {
            Ok(Some((checkout, attempt))) => {
                job.attempt = Some(attempt);
                job.state = State::Running;
                job.started_at_ms = Some(now());
                ready.push((job.clone(), checkout));
            }
            Ok(None) => continue,
            Err(error) => job.finish(Err(error)),
        }
        changed = true;
    }
    (changed, ready)
}

pub(crate) fn drive(source: &Workspace, runner: &Runner) -> Result<Queue> {
    if !source.data.join(FILE).try_exists()? {
        return Ok(Queue::default());
    }
    let owner = lock(source)?;
    let mut queue = Queue::read(source)?;
    let reconciliation = reconcile_running(source, &mut queue);
    let (claimed, ready) = claim_waiting(source, &mut queue);
    if reconciliation || claimed {
        save(source, &queue)?;
    }
    drop(owner);
    for (job, checkout) in ready {
        let source = source.clone();
        let runner = runner.clone();
        let id = job.id;
        // Blocking model clients run on independent threads. The inherited relay lease
        // continues counting the slot if Studio exits while the AEP subprocess is alive.
        if let Err(error) = thread::Builder::new()
            .name(format!("aep-task-{id}"))
            .spawn({
                let source = source.clone();
                move || {
                    let result = run(&checkout, &runner, &job);
                    let _ = finish(&source, id, result);
                    drop(checkout);
                }
            })
        {
            finish(&source, id, Err(error.into()))?;
        }
    }
    Ok(queue)
}

pub(crate) fn archive(session: &Path, out: &Path) -> Result<()> {
    let Some(id) = session.file_name().and_then(|name| name.to_str()) else {
        return Ok(());
    };
    let Some(data) = session
        .parent()
        .filter(|path| path.file_name().is_some_and(|name| name == "lifecycle"))
        .and_then(Path::parent)
    else {
        return Ok(());
    };
    let queue = Queue::read_path(data)?;
    let jobs = queue
        .jobs
        .into_iter()
        .filter(|job| job.session == id)
        .collect::<Vec<_>>();
    if !jobs.is_empty() {
        lifecycle::save(out, FILE, &jobs)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aep::workspace::git;
    use std::path::PathBuf;

    fn repository(path: &Path) -> Result<Workspace> {
        fs::create_dir_all(path)?;
        git(path, &["init", "-q", "--template="])?;
        Workspace::open(path)
    }

    fn add(source: &Workspace, id: &str) -> Result<PathBuf> {
        let root = source.data.join("lifecycle").join(id);
        let ws = repository(&root.join("workspace/repo"))?;
        let fixture = lifecycle::Fixture {
            source: source.root.clone(),
            studio: true,
            real_task: true,
            authored: true,
            repo: "owner/repo".into(),
            task: id.into(),
            base: "main".into(),
            base_sha: "base".into(),
            spec: id.into(),
            actor: "operator".into(),
        };
        let out = root.join("evidence");
        fs::create_dir(&out)?;
        lifecycle::save(&out, "fixture.json", &fixture)?;
        enqueue(
            source,
            &ws,
            &fixture,
            format!("relay-{id}"),
            Action::Start,
            String::new(),
        )?;
        Ok(root)
    }

    #[test]
    fn fifo_limit_survives_restart_and_claims_do_not_duplicate() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let source = repository(dir.path())?;
        let a = add(&source, "studio-a")?;
        let b = add(&source, "studio-b")?;
        let mut queue = Queue::read(&source)?;
        let (changed, first) = claim_waiting(&source, &mut queue);
        assert!(changed);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].0.session, "studio-a");
        let expected = first[0].0.id;
        save(&source, &queue)?;
        let mut reopened = Queue::read(&source)?;
        assert!(!reconcile_running(&source, &mut reopened));
        assert_eq!(reopened.latest(&b).unwrap().state, State::Waiting);
        assert!(claim_waiting(&source, &mut reopened).1.is_empty());

        set_limit(&source, 2)?;
        reopened = Queue::read(&source)?;
        let (_, second) = claim_waiting(&source, &mut reopened);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].0.session, "studio-b");
        save(&source, &reopened)?;
        set_limit(&source, 1)?;

        let mut reopened = Queue::read(&source)?;
        assert_eq!(reopened.running(), 2);
        assert!(claim_waiting(&source, &mut reopened).1.is_empty());
        drop(first);
        drop(second);
        assert!(reconcile_running(&source, &mut reopened));
        assert_eq!(reopened.latest(&a).unwrap().id, expected);
        assert_eq!(reopened.latest(&a).unwrap().state, State::Waiting);
        let (_, retry_before_launch) = claim_waiting(&source, &mut reopened);
        assert_eq!(retry_before_launch[0].0.id, expected);
        assert_eq!(
            retry_before_launch[0].0.attempt.as_deref(),
            Some("relay-studio-a-r1-1")
        );
        Ok(())
    }

    #[test]
    fn queue_cannot_duplicate_a_session_and_archive_only_exports_matching_jobs() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let source = repository(dir.path())?;
        let a = add(&source, "studio-a")?;
        add(&source, "studio-b")?;
        let out = a.join("evidence");
        let fixture = lifecycle::load_session(&a).context("missing task fixture")?;
        let checkout = Workspace::open(&a.join("workspace/repo"))?;
        assert!(
            enqueue(
                &source,
                &checkout,
                &fixture,
                "relay-studio-a".into(),
                Action::Start,
                String::new(),
            )
            .is_err()
        );
        archive(&a, &out)?;
        let jobs: Vec<Job> = serde_json::from_slice(&fs::read(out.join(FILE))?)?;
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].task, "studio-a");
        Ok(())
    }

    #[test]
    fn returning_to_a_finished_executor_releases_its_slot_without_a_replay() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let source = repository(dir.path())?;
        let a = add(&source, "studio-a")?;
        let b = add(&source, "studio-b")?;
        let mut queue = Queue::read(&source)?;
        let (_, first) = claim_waiting(&source, &mut queue);
        let (job, checkout) = &first[0];
        let attempt = job.attempt.as_deref().context("missing claimed attempt")?;
        let relay = checkout.ws.data.join("relays").join(&job.relay);
        let run = checkout.ws.data.join("runs").join(attempt);
        fs::create_dir_all(&run)?;
        lifecycle::save(
            &relay,
            "relay.json",
            &serde_json::json!({
                "id": job.relay, "task": {"name": job.task, "sha": job.spec}, "model": job.model,
                "attempts":[{"id":attempt,"round":"First","decision":null}]
            }),
        )?;
        lifecycle::save(
            &run,
            "evidence.json",
            &serde_json::json!({
                "status": "awaiting_human_review", "check_exit_code": 0, "review_status": "findings_recorded"
            }),
        )?;
        save(&source, &queue)?;
        drop(first);

        let mut reopened = Queue::read(&source)?;
        assert!(reconcile_running(&source, &mut reopened));
        assert_eq!(reopened.latest(&a).unwrap().state, State::Passed);
        let (_, next) = claim_waiting(&source, &mut reopened);
        assert_eq!(next[0].0.id, reopened.latest(&b).unwrap().id);
        assert_eq!(
            reopened.latest(&a).unwrap().attempt,
            queue.latest(&a).unwrap().attempt
        );
        Ok(())
    }
}
