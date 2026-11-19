use crate::{
    Runner, lifecycle,
    relay::{Lease, Round, Task},
};
use aep::{
    cancel::{Cancellation, Cancelled},
    workspace::{Workspace, status},
};
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
    Cancelling,
    Cancelled,
    Passed,
    Failed(String),
}

impl State {
    pub fn active(&self) -> bool {
        self == &Self::Waiting || self.occupies_slot()
    }

    fn occupies_slot(&self) -> bool {
        matches!(self, Self::Running | Self::Cancelling)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct Job {
    pub id: uuid::Uuid,
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
    #[serde(default)]
    priority: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cancelled_at_ms: Option<u128>,
}

impl Job {
    fn finish(&mut self, result: Result<()>) {
        self.state = match result {
            Ok(()) => State::Passed,
            Err(error) if error.is::<Cancelled>() => {
                self.cancelled_at_ms.get_or_insert_with(now);
                State::Cancelled
            }
            Err(error) => State::Failed(format!("{error:#}")),
        };
        self.finished_at_ms = Some(now());
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct Queue {
    pub limit: usize,
    #[serde(default)]
    pub paused: bool,
    jobs: Vec<Job>,
}

impl Default for Queue {
    fn default() -> Self {
        Self {
            limit: 1,
            paused: false,
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
            .filter(|job| job.state.occupies_slot())
            .count()
    }

    pub fn position(&self, job: &Job) -> Option<usize> {
        self.waiting()
            .into_iter()
            .position(|index| self.jobs[index].id == job.id)
            .map(|index| index + 1)
    }

    fn priority(&self, index: usize) -> usize {
        self.jobs[index].priority.unwrap_or(index)
    }

    fn waiting(&self) -> Vec<usize> {
        let mut ordered = self
            .jobs
            .iter()
            .enumerate()
            .filter_map(|(index, job)| (job.state == State::Waiting).then_some(index))
            .collect::<Vec<_>>();
        ordered.sort_by_key(|index| (self.priority(*index), *index));
        ordered
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

pub(crate) fn set_paused(source: &Workspace, paused: bool) -> Result<()> {
    let _lock = lock(source)?;
    let mut queue = Queue::read(source)?;
    queue.paused = paused;
    save(source, &queue)
}

#[derive(Clone, Copy)]
pub(crate) enum Direction {
    Earlier,
    Later,
}

pub(crate) fn move_waiting(source: &Workspace, id: uuid::Uuid, direction: Direction) -> Result<()> {
    let _lock = lock(source)?;
    let mut queue = Queue::read(source)?;
    let waiting = queue.waiting();
    let position = waiting
        .iter()
        .position(|index| queue.jobs[*index].id == id)
        .context("只可调整仍在等待的队列项")?;
    let adjacent = match direction {
        Direction::Earlier => position.checked_sub(1).and_then(|index| waiting.get(index)),
        Direction::Later => waiting.get(position + 1),
    }
    .context("没有相邻的排队项")?;
    let index = waiting[position];
    let current_priority = queue.priority(index);
    queue.jobs[index].priority = Some(queue.priority(*adjacent));
    queue.jobs[*adjacent].priority = Some(current_priority);
    save(source, &queue)
}

pub(crate) fn cancel(source: &Workspace, id: uuid::Uuid, expected: State) -> Result<()> {
    let _lock = lock(source)?;
    let mut queue = Queue::read(source)?;
    let job = queue
        .jobs
        .iter_mut()
        .find(|job| job.id == id)
        .context("找不到队列项")?;
    if matches!(job.state, State::Cancelling | State::Cancelled) {
        return Ok(());
    }
    ensure!(job.state == expected, "本轮状态已变化，请刷新后确认");
    match job.state {
        State::Waiting => job.finish(Err(Cancelled.into())),
        State::Running => {
            let (ws, _) = open_checkout(source, job)?;
            let attempt = job.attempt.as_deref().context("缺少本轮运行 ID")?;
            let cancellation = Cancellation::new(&ws.data, attempt)?;
            ensure!(cancellation.request()?, "本轮已结束，请刷新任务状态");
            job.cancelled_at_ms = Some(now());
            job.state = State::Cancelling;
        }
        _ => anyhow::bail!("本轮已结束，请刷新任务状态"),
    }
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
    let priority = queue.jobs.len();
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
        priority: Some(priority),
        cancelled_at_ms: None,
    });
    save(source, &queue)
}

struct Checkout {
    ws: Workspace,
    fixture: lifecycle::Fixture,
    lease: Lease,
}

fn open_checkout(source: &Workspace, job: &Job) -> Result<(Workspace, lifecycle::Fixture)> {
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
    Ok((ws, fixture))
}

fn claim(source: &Workspace, job: &Job) -> Result<Option<Checkout>> {
    let (ws, fixture) = open_checkout(source, job)?;
    let Some(lease) = Lease::acquire(&ws, &job.relay)? else {
        return Ok(None);
    };
    Ok(Some(Checkout { ws, fixture, lease }))
}

fn reconcile(job: &mut Job, checkout: &Checkout) -> Result<()> {
    let attempt = job.attempt.as_deref().context("排队任务缺少运行 ID")?;
    let record = status(&checkout.ws.data, attempt).unwrap_or_default();
    if record["status"] == "cancelled"
        || (Cancellation::new(&checkout.ws.data, attempt)?.requested()?
            && record["status"] != "awaiting_human_review")
    {
        job.finish(Err(Cancelled.into()));
        return Ok(());
    }
    if !checkout.lease.has_attempt(attempt)? {
        // Studio exited before the relay recorded a run. Dispatch can reuse the pinned Spec.
        job.state = State::Waiting;
        job.started_at_ms = None;
        job.attempt = None;
        return Ok(());
    }
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
    let attempt = job.attempt.as_deref().context("排队任务缺少运行 ID")?;
    let cancellation = Cancellation::new(&ws.data, attempt)?;
    cancellation.check()?;
    match job.action {
        Action::Start => {
            if fixture.authored {
                fixture.dispatch_authored(ws, runner)?;
            }
            cancellation.check()?;
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
        .find(|job| job.id == id && job.state.occupies_slot())
    {
        let result = if job.state == State::Cancelling && result.is_err() {
            // A cancellation can arrive during dispatch, before a Run exists to record it.
            Err(Cancelled.into())
        } else {
            result
        };
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
        .filter(|job| job.state.occupies_slot())
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
    if queue.paused {
        return (false, Vec::new());
    }
    let free = queue.limit.saturating_sub(queue.running());
    let mut ready = Vec::new();
    let mut changed = false;
    for index in queue.waiting() {
        if ready.len() == free {
            break;
        }
        let job = &mut queue.jobs[index];
        let result = claim(source, job).and_then(|checkout| {
            checkout
                .map(|checkout| {
                    let attempt = checkout.lease.planned_run(job.action.round())?;
                    Cancellation::new(&checkout.ws.data, &attempt)?.clear_unlaunched()?;
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
        assert!(cancel(&source, job.id, State::Running).is_err());
        assert!(!Cancellation::new(&checkout.ws.data, attempt)?.requested()?);
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

    #[test]
    fn pause_reorder_and_waiting_cancellation_survive_reopen() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let source = repository(dir.path())?;
        let a = add(&source, "studio-a")?;
        let b = add(&source, "studio-b")?;
        let c = add(&source, "studio-c")?;
        let original = Queue::read(&source)?;
        let a_id = original.latest(&a).unwrap().id;
        let b_id = original.latest(&b).unwrap().id;
        let c_id = original.latest(&c).unwrap().id;
        let mut legacy = serde_json::to_value(original)?;
        legacy.as_object_mut().unwrap().remove("paused");
        for job in legacy["jobs"].as_array_mut().unwrap() {
            job.as_object_mut().unwrap().remove("priority");
        }
        lifecycle::save(&source.data, FILE, &legacy)?;
        assert!(!Queue::read(&source)?.paused);

        set_paused(&source, true)?;
        move_waiting(&source, c_id, Direction::Earlier)?;
        move_waiting(&source, a_id, Direction::Later)?;
        cancel(&source, b_id, State::Waiting)?;
        let mut reopened = Queue::read(&source)?;
        assert!(reopened.paused);
        let cancelled = reopened.latest(&b).unwrap();
        assert_eq!(cancelled.state, State::Cancelled);
        assert!(cancelled.attempt.is_none());
        assert!(cancelled.cancelled_at_ms.is_some());
        assert!(!checkout_data(&b)?.join("run-control").exists());
        assert!(move_waiting(&source, b_id, Direction::Earlier).is_err());
        assert_eq!(reopened.position(reopened.latest(&c).unwrap()), Some(1));
        assert_eq!(reopened.position(reopened.latest(&a).unwrap()), Some(2));
        assert!(claim_waiting(&source, &mut reopened).1.is_empty());

        set_paused(&source, false)?;
        let mut reopened = Queue::read(&source)?;
        assert_eq!(claim_waiting(&source, &mut reopened).1[0].0.id, c_id);
        Ok(())
    }

    fn checkout_data(session: &Path) -> Result<PathBuf> {
        Ok(Workspace::open(&session.join("workspace/repo"))?.data)
    }

    #[test]
    fn cancelling_keeps_the_slot_until_the_lease_is_released() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let source = repository(dir.path())?;
        let a = add(&source, "studio-a")?;
        let b = add(&source, "studio-b")?;
        let mut queue = Queue::read(&source)?;
        let (_, first) = claim_waiting(&source, &mut queue);
        let (job, checkout) = &first[0];
        let cancelled_id = job.id;
        let attempt = job.attempt.as_deref().unwrap().to_owned();
        let marker = Cancellation::new(&checkout.ws.data, &attempt)?;
        save(&source, &queue)?;
        assert!(cancel(&source, cancelled_id, State::Waiting).is_err());
        assert!(!marker.requested()?);
        cancel(&source, cancelled_id, State::Running)?;

        let mut reopened = Queue::read(&source)?;
        assert_eq!(reopened.latest(&a).unwrap().state, State::Cancelling);
        assert_eq!(reopened.running(), 1);
        assert!(marker.requested()?);
        assert!(!checkout_data(&b)?.join("run-control").exists());
        assert!(!reconcile_running(&source, &mut reopened));
        assert!(claim_waiting(&source, &mut reopened).1.is_empty());
        drop(first);

        set_paused(&source, true)?;
        let mut reopened = Queue::read(&source)?;
        assert!(reconcile_running(&source, &mut reopened));
        let cancelled = reopened.latest(&a).unwrap();
        assert_eq!(cancelled.id, cancelled_id);
        assert_eq!(cancelled.state, State::Cancelled);
        assert_eq!(cancelled.attempt.as_deref(), Some(attempt.as_str()));
        assert!(cancelled.finished_at_ms.is_some());
        assert!(claim_waiting(&source, &mut reopened).1.is_empty());
        save(&source, &reopened)?;

        let ws = Workspace::open(&a.join("workspace/repo"))?;
        let fixture = lifecycle::load_session(&a).context("missing task fixture")?;
        enqueue(
            &source,
            &ws,
            &fixture,
            "relay-studio-a".into(),
            Action::Start,
            String::new(),
        )?;
        set_paused(&source, false)?;
        let mut reopened = Queue::read(&source)?;
        let (_, survivor) = claim_waiting(&source, &mut reopened);
        assert_eq!(survivor[0].0.id, reopened.latest(&b).unwrap().id);
        set_limit(&source, 2)?;
        reopened.limit = Queue::read(&source)?.limit;
        let (_, explicit_retry) = claim_waiting(&source, &mut reopened);
        assert_eq!(
            explicit_retry[0].0.attempt.as_deref(),
            Some(attempt.as_str())
        );
        assert!(!marker.requested()?);
        assert_eq!(reopened.jobs[0].state, State::Cancelled);
        Ok(())
    }
}
