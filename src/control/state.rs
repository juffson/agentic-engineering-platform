//! Single-project control-plane state. Only identifiers and execution summaries cross the wire.
use aep::{
    deploy_keys::{
        Change, Inventory, Receipt, Request, RollingRotation, RotationWorker, WorkerKeys,
    },
    workspace::ident,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub ticket: String,
    pub name: String,
    pub repository: String,
    pub spec: String,
    pub target: String,
    pub baseline: String,
    pub timeout: u64,
    pub max_attempts: usize,
}

pub fn sha(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Task {
    fn validate(&self) -> Result<()> {
        ident(&self.ticket, 63)?;
        ident(&self.name, 63)?;
        ensure!(
            self.repository.len() <= 200
                && aep::delivery::slug(&format!("https://github.com/{}", self.repository))?
                    == self.repository,
            "invalid control repository"
        );
        ensure!(
            sha(&self.spec) && sha(&self.baseline),
            "pin both Spec and baseline commit SHAs"
        );
        ensure!(
            self.target.len() <= 160
                && !self.target.starts_with('-')
                && !self.target.contains("..")
                && !self.target.contains("//")
                && self.target.split('/').all(|part| !part.is_empty()
                    && !part.starts_with('.')
                    && !part.ends_with('.')
                    && !part.ends_with(".lock"))
                && self
                    .target
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(&b)),
            "invalid Git target branch"
        );
        ensure!(
            (1..=3600).contains(&self.timeout),
            "timeout must be between 1 and 3600 seconds"
        );
        ensure!(
            (1..=8).contains(&self.max_attempts),
            "max attempts must be between 1 and 8"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub codex: String,
    pub claude: String,
}

impl Profile {
    fn validate(&self) -> Result<()> {
        ensure!(
            [&self.codex, &self.claude].into_iter().all(|version| {
                !version.is_empty()
                    && version.len() <= 220
                    && !version.chars().any(char::is_control)
            }),
            "invalid client version"
        );
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Preparing,
    Implementing,
    Checking,
    Reviewing,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Passed,
    Failed,
    TimedOut,
    Cancelled,
    Crashed,
    Blocked,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Progress {
    Phase {
        phase: Phase,
        candidate: Option<String>,
    },
    Exit {
        outcome: Outcome,
        candidate: Option<String>,
    },
}

impl Progress {
    fn candidate(&self) -> Option<&str> {
        match self {
            Self::Phase { candidate, .. } | Self::Exit { candidate, .. } => candidate.as_deref(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub sequence: usize,
    pub unix_seconds: u64,
    pub progress: Progress,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Queued,
    Running,
    Interrupted,
    CancelPending,
    Cancelled,
    Failed,
    Blocked,
    AwaitingApproval,
    Approved,
    Publishing,
    Published,
    Superseded,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Approval {
    pub actor: String,
    pub spec: String,
    pub candidate: String,
    pub report_sha256: String,
    pub approved_at: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Attempt {
    pub id: String,
    pub task: Task,
    pub stage: Stage,
    pub predecessor: Option<String>,
    pub reverify: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<String>,
    pub profile: Option<Profile>,
    pub expires_at: Option<u64>,
    pub events: Vec<Event>,
    pub approval: Option<Approval>,
    pub pull_request: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fence: Option<String>,
}

impl Attempt {
    pub fn candidate(&self) -> Option<&str> {
        self.events
            .iter()
            .rev()
            .find_map(|event| event.progress.candidate())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Lease {
    pub run: String,
    pub fence: String,
    pub task: Task,
    pub predecessor: Option<String>,
    pub reverify: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Device {
    pub id: String,
    pub installation: String,
    pub last_seen: u64,
    pub profile: Profile,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub online: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct State {
    pub device: Option<Device>,
    #[serde(default)]
    pub workers: std::collections::BTreeMap<String, Device>,
    pub attempts: Vec<Attempt>,
    #[serde(default)]
    pub queue: Queue,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub deploy_keys: std::collections::BTreeMap<String, WorkerKeys>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_rotation: Option<RollingRotation>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct Queue {
    pub paused: bool,
    pub order: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Dispatch {
        task: Task,
        revise: bool,
    },
    Status,
    QueuePause {
        paused: bool,
    },
    QueueMove {
        run: String,
        adjacent: String,
        earlier: bool,
    },
    Cancel {
        ticket: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_run: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_stage: Option<Stage>,
    },
    Retry {
        ticket: String,
        reverify: bool,
    },
    Approve {
        ticket: String,
        run: String,
        actor: String,
        spec: String,
        candidate: String,
        report_sha256: String,
    },
    BeginPublication {
        ticket: String,
        run: String,
        spec: String,
        candidate: String,
        report_sha256: String,
    },
    Published {
        ticket: String,
        run: String,
        candidate: String,
        pr: u64,
    },
    Poll {
        installation: String,
        profile: Profile,
    },
    KeySync {
        installation: String,
        profile: Profile,
        repository: String,
        inventory: Inventory,
        receipt: Option<Receipt>,
    },
    KeyRequest {
        repository: String,
        worker: String,
        installation: String,
        change: Change,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rotation: Option<String>,
    },
    KeyRotationStart {
        repository: String,
    },
    KeyRotationStop {
        repository: String,
        rotation: String,
    },
    Renew {
        run: String,
        fence: String,
    },
    Report {
        run: String,
        fence: String,
        event: Event,
    },
}

impl Operation {
    pub fn for_device(&self) -> bool {
        matches!(
            self,
            Self::Poll { .. } | Self::Renew { .. } | Self::Report { .. } | Self::KeySync { .. }
        )
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "result", content = "data", rename_all = "snake_case")]
pub enum Reply {
    Snapshot(State),
    Run(Box<Attempt>),
    Polled(Option<Lease>),
    Renewed { cancel: bool, stale: bool },
    Ack(usize),
    KeyWork(Option<Request>),
}

impl State {
    fn normalize_queue(&mut self) {
        // Attempt history stays in insertion order for retry and approval fencing.
        let mut queued = self
            .attempts
            .iter()
            .filter(|run| run.stage == Stage::Queued)
            .map(|run| run.id.as_str())
            .collect::<std::collections::HashSet<_>>();
        self.queue.order.retain(|id| queued.remove(id.as_str()));
        self.queue.order.extend(
            self.attempts
                .iter()
                .filter(|run| queued.contains(run.id.as_str()))
                .map(|run| run.id.clone()),
        );
    }

    fn migrate_workers(&mut self) {
        if let Some(device) = &self.device {
            self.workers
                .entry(device.id.clone())
                .or_insert_with(|| device.clone());
            for run in &mut self.attempts {
                if run.worker.is_none() && (run.profile.is_some() || run.reverify) {
                    run.worker = Some(device.id.clone());
                }
            }
        }
    }

    fn snapshot(&self, now: u64, ttl: u64) -> Reply {
        let mut snapshot = self.clone();
        for worker in snapshot.workers.values_mut() {
            worker.online = Some(now.saturating_sub(worker.last_seen) < ttl);
        }
        for run in &mut snapshot.attempts {
            run.fence = None;
        }
        Reply::Snapshot(snapshot)
    }

    fn move_queued(&mut self, run: &str, adjacent: &str, earlier: bool) -> Result<()> {
        let order = &mut self.queue.order;
        let index = order
            .iter()
            .position(|id| id == run)
            .context("Run is no longer queued")?;
        let neighbor = if earlier {
            index.checked_sub(1)
        } else {
            index.checked_add(1)
        };
        let neighbor = neighbor
            .filter(|&i| order.get(i).is_some_and(|id| id == adjacent))
            .context("Queue order changed; refresh before moving this Run")?;
        order.swap(index, neighbor);
        Ok(())
    }

    fn expire(&mut self, now: u64) {
        for run in &mut self.attempts {
            if run.stage == Stage::Running && run.expires_at.is_some_and(|expiry| expiry <= now) {
                run.stage = Stage::Interrupted;
            }
        }
    }

    fn last(&self, ticket: &str) -> Option<&Attempt> {
        self.attempts
            .iter()
            .rev()
            .find(|run| run.task.ticket == ticket)
    }

    fn last_mut(&mut self, ticket: &str) -> Result<&mut Attempt> {
        self.attempts
            .iter_mut()
            .rev()
            .find(|run| run.task.ticket == ticket)
            .context("unknown ticket")
    }

    fn run_for_fence(&mut self, id: &str, fence: &str, device: &str) -> Result<&mut Attempt> {
        let run = self
            .attempts
            .iter_mut()
            .find(|run| run.id == id)
            .context("unknown Run")?;
        ensure!(
            run.worker.as_deref() == Some(device) && run.fence.as_deref() == Some(fence),
            "stale lease or wrong Worker"
        );
        Ok(run)
    }

    fn enqueue(
        &mut self,
        task: Task,
        predecessor: Option<String>,
        worker: Option<String>,
    ) -> Reply {
        let run = Attempt {
            id: uuid::Uuid::new_v4().simple().to_string(),
            task,
            stage: Stage::Queued,
            predecessor,
            reverify: worker.is_some(),
            worker,
            profile: None,
            expires_at: None,
            events: Vec::new(),
            approval: None,
            pull_request: None,
            fence: None,
        };
        self.attempts.push(run.clone());
        Reply::Run(Box::new(run))
    }

    fn dispatch(&mut self, task: Task, revise: bool) -> Result<Reply> {
        task.validate()?;
        ensure!(
            self.attempts
                .first()
                .is_none_or(|a| a.task.repository == task.repository)
                && self
                    .deploy_keys
                    .values()
                    .all(|entry| entry.repository == task.repository),
            "control plane already belongs to another repository"
        );
        if let Some(previous) = self.last(&task.ticket) {
            if previous.task == task {
                return Ok(Reply::Run(Box::new(public_run(previous))));
            }
            ensure!(
                revise && previous.task.spec != task.spec,
                "ticket already pins another Spec"
            );
            ensure!(
                matches!(
                    previous.stage,
                    Stage::Queued
                        | Stage::Interrupted
                        | Stage::Failed
                        | Stage::Blocked
                        | Stage::Cancelled
                        | Stage::AwaitingApproval
                        | Stage::Approved
                ),
                "finish cancellation or reconcile publication before revising the Spec"
            );
            let previous = self.last_mut(&task.ticket)?;
            previous.stage = Stage::Superseded;
        }
        Ok(self.enqueue(task, None, None))
    }

    fn retry(&mut self, ticket: &str, reverify: bool) -> Result<Reply> {
        let previous = self.last(ticket).context("unknown ticket")?;
        ensure!(
            matches!(
                previous.stage,
                Stage::Interrupted | Stage::Failed | Stage::Blocked | Stage::Cancelled
            ),
            "only interrupted, failed, blocked or cancelled runs can be retried"
        );
        let used = self
            .attempts
            .iter()
            .filter(|a| a.task.ticket == ticket && a.task.spec == previous.task.spec)
            .count();
        ensure!(
            used < previous.task.max_attempts,
            "attempt budget exhausted"
        );
        let worker = if reverify {
            Some(
                previous
                    .worker
                    .clone()
                    .context("the previous Run has no Worker for its local candidate")?,
            )
        } else {
            None
        };
        Ok(self.enqueue(previous.task.clone(), Some(previous.id.clone()), worker))
    }

    fn touch_worker(
        &mut self,
        device: &str,
        installation: String,
        profile: Profile,
        now: u64,
    ) -> Result<()> {
        profile.validate()?;
        ident(&installation, 64)?;
        ensure!(
            self.workers
                .get(device)
                .is_none_or(|saved| saved.installation == installation)
                && self
                    .workers
                    .values()
                    .all(|saved| saved.id == device || saved.installation != installation),
            "a different installation is already registered for this device"
        );
        let worker = Device {
            id: device.into(),
            installation,
            last_seen: now,
            profile: profile.clone(),
            online: None,
        };
        self.workers.insert(device.into(), worker.clone());
        if self
            .device
            .as_ref()
            .is_none_or(|legacy| legacy.id == device)
        {
            self.device = Some(worker);
        }
        Ok(())
    }

    fn active_run(&self, device: &str) -> Option<&Attempt> {
        self.attempts.iter().find(|run| {
            run.worker.as_deref() == Some(device)
                && matches!(run.stage, Stage::Running | Stage::CancelPending)
        })
    }

    fn start_key_rotation(&mut self, repository: String, now: u64, ttl: u64) -> Result<()> {
        ensure!(
            self.key_rotation
                .as_ref()
                .and_then(RollingRotation::current)
                .is_none(),
            "a rolling Deploy Key rotation is already active"
        );
        ensure!(
            self.deploy_keys.len() >= 2,
            "rolling rotation needs at least two configured Workers"
        );
        let mut public_keys = std::collections::HashSet::new();
        let mut workers = Vec::with_capacity(self.deploy_keys.len());
        for (worker, entry) in &self.deploy_keys {
            ensure!(
                entry.repository == repository,
                "the selected repository does not match the Workers"
            );
            let key = entry
                .inventory
                .active
                .as_ref()
                .context("set up each Worker's Deploy Key first")?;
            ensure!(
                public_keys.insert(&key.public),
                "each Worker must have its own Deploy Key"
            );
            ensure!(
                entry.request.is_none()
                    && entry.inventory.staged.is_none()
                    && entry.inventory.retiring.is_none(),
                "finish pending Deploy Key operations before starting a rolling rotation"
            );
            ensure!(
                self.workers
                    .get(worker)
                    .is_some_and(|device| now.saturating_sub(device.last_seen) < ttl),
                "bring each configured Worker online before starting a rolling rotation"
            );
            workers.push(RotationWorker {
                worker: worker.clone(),
                installation: entry.installation.clone(),
                original: key.fingerprint.clone(),
                replacement: None,
            });
        }
        self.key_rotation = Some(RollingRotation {
            id: uuid::Uuid::new_v4().simple().to_string(),
            repository,
            workers,
            next: 0,
            stopped: false,
        });
        Ok(())
    }

    pub fn authorize_key_change(
        &self,
        repository: &str,
        worker: &str,
        installation: &str,
        rotation: Option<&str>,
        change: &Change,
    ) -> Result<&WorkerKeys> {
        let entry = self
            .deploy_keys
            .get(worker)
            .filter(|entry| entry.repository == repository && entry.installation == installation)
            .context("the Worker does not manage Deploy Keys for this repository")?;
        if let Some((plan, current)) = self
            .key_rotation
            .as_ref()
            .and_then(|plan| plan.current().map(|current| (plan, current)))
        {
            ensure!(
                rotation == Some(plan.id.as_str())
                    && current.worker == worker
                    && current.installation == installation,
                "use the current Worker in this rolling Deploy Key rotation"
            );
            match change {
                Change::Prepare | Change::Activate { .. } => ensure!(
                    entry
                        .inventory
                        .active
                        .as_ref()
                        .is_some_and(|key| key.fingerprint == current.original),
                    "the current Worker's original Deploy Key changed"
                ),
                Change::Forget { fingerprint } => ensure!(
                    *fingerprint == current.original
                        && entry
                            .inventory
                            .active
                            .as_ref()
                            .is_some_and(|key| key.fingerprint != current.original),
                    "activate a verified replacement before retiring this Worker's original key"
                ),
                Change::Verify { .. } => (),
            }
        } else {
            ensure!(
                rotation.is_none(),
                "the rolling Deploy Key rotation changed"
            );
        }
        change.validate(&entry.inventory)?;
        ensure!(
            entry
                .request
                .as_ref()
                .is_none_or(|request| request.change == *change),
            "a Deploy Key operation is already pending"
        );
        if let Change::Activate { .. } = change {
            let staged = &entry
                .inventory
                .staged
                .as_ref()
                .context("missing staged Deploy Key")?
                .public;
            ensure!(
                self.deploy_keys.iter().all(|(id, peer)| id == worker
                    || [
                        &peer.inventory.active,
                        &peer.inventory.staged,
                        &peer.inventory.retiring
                    ]
                    .into_iter()
                    .flatten()
                    .all(|key| &key.public != staged)),
                "each Worker must have its own Deploy Key"
            );
        }
        if let Change::Forget { .. } = change {
            ensure!(
                entry.verified_at.is_some(),
                "verify the new Deploy Key before revoking the old one"
            );
            let old = &entry
                .inventory
                .retiring
                .as_ref()
                .context("missing retiring Deploy Key")?
                .public;
            ensure!(
                self.deploy_keys.iter().all(|(id, peer)| id == worker
                    || [&peer.inventory.active, &peer.inventory.staged]
                        .into_iter()
                        .flatten()
                        .all(|key| &key.public != old)),
                "another Worker still uses the old Deploy Key"
            );
        }
        Ok(entry)
    }

    fn key_sync(
        &mut self,
        device: &str,
        repository: String,
        inventory: Inventory,
        receipt: Option<Receipt>,
        now: u64,
    ) -> Result<Reply> {
        ensure!(
            repository.len() <= 200
                && aep::delivery::slug(&format!("https://github.com/{repository}"))? == repository
                && self
                    .attempts
                    .first()
                    .is_none_or(|a| a.task.repository == repository)
                && self
                    .deploy_keys
                    .values()
                    .all(|entry| entry.repository == repository),
            "Deploy Keys belong to another repository"
        );
        inventory.validate()?;
        if let Some(receipt) = &receipt {
            ident(&receipt.id, 64)?;
            ensure!(
                receipt
                    .error
                    .as_ref()
                    .is_none_or(|error| error.len() <= 512 && !error.chars().any(char::is_control)),
                "invalid Deploy Key receipt"
            );
        }
        let idle = self.active_run(device).is_none();
        let entry = self
            .deploy_keys
            .entry(device.into())
            .or_insert_with(|| WorkerKeys {
                repository,
                installation: self.workers[device].installation.clone(),
                inventory: Inventory::default(),
                request: None,
                last: None,
                verified_at: None,
            });
        if entry.inventory.active != inventory.active {
            entry.verified_at = None;
        }
        let mut retired = None;
        if let Some(receipt) = receipt
            && entry
                .request
                .as_ref()
                .is_some_and(|request| request.id == receipt.id)
        {
            let request = entry.request.take().context("missing Deploy Key request")?;
            if receipt.error.is_none() && request.change.verifies_active(&inventory) {
                entry.verified_at = Some(now);
            }
            if receipt.error.is_some() && matches!(request.change, Change::Verify { .. }) {
                entry.verified_at = None;
            }
            if receipt.error.is_none()
                && let Change::Forget { fingerprint } = request.change
                && inventory.retiring.is_none()
                && inventory.active == entry.inventory.active
                && entry.verified_at.is_some()
            {
                retired = inventory
                    .active
                    .as_ref()
                    .map(|key| (fingerprint, key.fingerprint.clone()));
            }
            entry.last = Some(receipt);
        }
        entry.inventory = inventory;
        if let Some((old, replacement)) = retired
            && let Some(plan) = &mut self.key_rotation
            && plan.current().is_some_and(|current| {
                current.worker == device
                    && current.installation == entry.installation
                    && current.original == old
            })
        {
            plan.workers[plan.next].replacement = Some(replacement);
            plan.next += 1;
        }
        Ok(Reply::KeyWork(if idle {
            entry.request.clone()
        } else {
            None
        }))
    }

    fn poll(
        &mut self,
        device: &str,
        installation: String,
        profile: Profile,
        now: u64,
        ttl: u64,
    ) -> Result<Reply> {
        self.touch_worker(device, installation, profile.clone(), now)?;
        if let Some(run) = self.active_run(device) {
            // Re-deliver an unacknowledged lease, keeping the same Run ID and fence.
            return Ok(Reply::Polled(Some(lease(run)?)));
        }
        if self.queue.paused
            || (!self.deploy_keys.is_empty()
                && self.deploy_keys.get(device).is_none_or(|entry| {
                    entry.inventory.active.is_none() || entry.request.is_some()
                }))
        {
            return Ok(Reply::Polled(None));
        }
        let next = self.queue.order.iter().find_map(|id| {
            self.attempts.iter().position(|run| {
                run.id == *id
                    && match run.worker.as_deref() {
                        Some(owner) => owner == device,
                        None => self
                            .key_rotation
                            .as_ref()
                            .and_then(RollingRotation::current)
                            .is_none_or(|current| current.worker != device),
                    }
            })
        });
        let Some(index) = next else {
            return Ok(Reply::Polled(None));
        };
        let next = &mut self.attempts[index];
        next.stage = Stage::Running;
        next.worker = Some(device.into());
        next.fence = Some(uuid::Uuid::new_v4().simple().to_string());
        next.expires_at = Some(now + ttl);
        next.profile = Some(profile);
        Ok(Reply::Polled(Some(lease(next)?)))
    }

    fn report(&mut self, id: &str, fence: &str, device: &str, event: Event) -> Result<Reply> {
        let run = self.run_for_fence(id, fence, device)?;
        ensure!(event.sequence > 0, "event sequence starts at 1");
        if let Some(previous) = run.events.get(event.sequence - 1) {
            ensure!(*previous == event, "conflicting replay of a recorded event");
            return Ok(Reply::Ack(event.sequence));
        }
        ensure!(event.sequence == run.events.len() + 1, "event sequence gap");
        let live = run.stage == Stage::Running;
        let cancelling = run.stage == Stage::CancelPending;
        let historical = matches!(run.stage, Stage::Interrupted | Stage::Superseded)
            && !run
                .events
                .last()
                .is_some_and(|event| matches!(event.progress, Progress::Exit { .. }));
        ensure!(live || cancelling || historical, "terminal Run");
        if let Some(candidate) = event.progress.candidate() {
            ensure!(sha(candidate), "invalid candidate SHA");
            ensure!(
                run.candidate().is_none_or(|previous| previous == candidate),
                "candidate changed within one Run"
            );
        }
        if let Progress::Exit { outcome, candidate } = &event.progress {
            ensure!(
                outcome != &Outcome::Passed || candidate.is_some(),
                "passing result needs a candidate"
            );
            // A cancellation request wins a race with success and still permits a late acknowledgement.
            run.stage = if cancelling {
                Stage::Cancelled
            } else if historical {
                run.stage
            } else {
                match outcome {
                    Outcome::Passed => Stage::AwaitingApproval,
                    Outcome::Cancelled => Stage::Cancelled,
                    Outcome::Crashed => Stage::Interrupted,
                    Outcome::Blocked => Stage::Blocked,
                    _ => Stage::Failed,
                }
            };
            run.expires_at = None;
        }
        let sequence = event.sequence;
        run.events.push(event);
        Ok(Reply::Ack(sequence))
    }

    pub fn apply(
        &mut self,
        operation: Operation,
        device: &str,
        now: u64,
        ttl: u64,
    ) -> Result<Reply> {
        self.expire(now);
        self.normalize_queue();
        match operation {
            Operation::Status => Ok(self.snapshot(now, ttl)),
            Operation::KeySync {
                installation,
                profile,
                repository,
                inventory,
                receipt,
            } => {
                self.touch_worker(device, installation, profile, now)?;
                self.key_sync(device, repository, inventory, receipt, now)
            }
            Operation::KeyRequest {
                repository,
                worker,
                installation,
                change,
                rotation,
            } => {
                ensure!(
                    self.workers.get(&worker).is_some_and(|device| {
                        device.installation == installation
                            && now.saturating_sub(device.last_seen) < ttl
                    }),
                    "the selected Worker is offline or its installation changed"
                );
                self.authorize_key_change(
                    &repository,
                    &worker,
                    &installation,
                    rotation.as_deref(),
                    &change,
                )?;
                let entry = self
                    .deploy_keys
                    .get_mut(&worker)
                    .context("missing Worker Deploy Keys")?;
                if entry.request.is_none() {
                    entry.request = Some(Request {
                        id: uuid::Uuid::new_v4().simple().to_string(),
                        change,
                    });
                }
                Ok(self.snapshot(now, ttl))
            }
            Operation::KeyRotationStart { repository } => {
                self.start_key_rotation(repository, now, ttl)?;
                Ok(self.snapshot(now, ttl))
            }
            Operation::KeyRotationStop {
                repository,
                rotation,
            } => {
                let plan = self
                    .key_rotation
                    .as_mut()
                    .filter(|plan| plan.id == rotation && plan.repository == repository)
                    .context("the rolling Deploy Key rotation changed")?;
                let current = plan.current().context("the rotation is already finished")?;
                ensure!(
                    self.deploy_keys[&current.worker].request.is_none(),
                    "wait for the current Deploy Key operation to finish"
                );
                plan.stopped = true;
                Ok(self.snapshot(now, ttl))
            }
            Operation::QueuePause { paused } => {
                self.queue.paused = paused;
                Ok(self.snapshot(now, ttl))
            }
            Operation::QueueMove {
                run,
                adjacent,
                earlier,
            } => {
                self.move_queued(&run, &adjacent, earlier)?;
                Ok(self.snapshot(now, ttl))
            }
            Operation::Dispatch { task, revise } => self.dispatch(task, revise),
            Operation::Retry { ticket, reverify } => self.retry(&ticket, reverify),
            Operation::Poll {
                installation,
                profile,
            } => self.poll(device, installation, profile, now, ttl),
            Operation::Renew { run, fence } => {
                let run = self.run_for_fence(&run, &fence, device)?;
                let cancel = run.stage == Stage::CancelPending;
                let stale = !matches!(run.stage, Stage::Running | Stage::CancelPending);
                if !stale {
                    run.expires_at = Some(now + ttl);
                }
                if let Some(worker) = self.workers.get_mut(device) {
                    worker.last_seen = now;
                }
                if let Some(legacy) = self.device.as_mut().filter(|worker| worker.id == device) {
                    legacy.last_seen = now;
                }
                Ok(Reply::Renewed { cancel, stale })
            }
            Operation::Report { run, fence, event } => self.report(&run, &fence, device, event),
            Operation::Cancel {
                ticket,
                expected_run,
                expected_stage,
            } => {
                let run = self.last_mut(&ticket)?;
                ensure!(
                    expected_run.is_none_or(|id| id == run.id),
                    "Run changed; refresh before cancelling"
                );
                ensure!(
                    expected_stage.is_none_or(|stage| stage == run.stage)
                        || matches!(run.stage, Stage::Cancelled | Stage::CancelPending),
                    "Run stage changed; review it before cancelling"
                );
                run.stage = match run.stage {
                    Stage::Running => Stage::CancelPending,
                    Stage::Interrupted if run.expires_at.is_some() => Stage::CancelPending,
                    Stage::Queued
                    | Stage::Interrupted
                    | Stage::AwaitingApproval
                    | Stage::Approved => Stage::Cancelled,
                    Stage::Cancelled | Stage::CancelPending => run.stage,
                    _ => anyhow::bail!("current stage cannot be cancelled"),
                };
                Ok(Reply::Run(Box::new(public_run(run))))
            }
            Operation::Approve {
                ticket,
                run: id,
                actor,
                spec,
                candidate,
                report_sha256,
            } => {
                let run = self.last_mut(&ticket)?;
                ensure!(
                    run.id == id
                        && matches!(run.stage, Stage::AwaitingApproval | Stage::Approved)
                        && run.task.spec == spec
                        && run.candidate() == Some(candidate.as_str()),
                    "approval must match the current passing Spec and candidate"
                );
                ensure!(
                    !actor.trim().is_empty()
                        && actor.len() <= 120
                        && !actor.chars().any(char::is_control),
                    "invalid approver"
                );
                ensure!(
                    report_sha256.len() == 64 && sha(&report_sha256),
                    "invalid report digest"
                );
                ensure!(
                    run.approval
                        .as_ref()
                        .is_none_or(|previous| previous.report_sha256 == report_sha256),
                    "evidence changed since approval"
                );
                run.stage = Stage::Approved;
                run.approval.get_or_insert(Approval {
                    actor,
                    spec,
                    candidate,
                    report_sha256,
                    approved_at: now,
                });
                Ok(Reply::Run(Box::new(public_run(run))))
            }
            Operation::BeginPublication {
                ticket,
                run,
                spec,
                candidate,
                report_sha256,
            } => {
                let current = self.last_mut(&ticket)?;
                ensure!(
                    current.id == run
                        && matches!(
                            current.stage,
                            Stage::Approved | Stage::Publishing | Stage::Published
                        ),
                    "stale Run or missing delivery approval"
                );
                ensure!(
                    current.approval.as_ref().is_some_and(|a| a.spec == spec
                        && a.candidate == candidate
                        && a.report_sha256 == report_sha256),
                    "publication differs from approval"
                );
                if current.stage != Stage::Published {
                    current.stage = Stage::Publishing;
                }
                Ok(Reply::Run(Box::new(public_run(current))))
            }
            Operation::Published {
                ticket,
                run,
                candidate,
                pr,
            } => {
                let current = self.last_mut(&ticket)?;
                ensure!(
                    current.id == run && current.candidate() == Some(candidate.as_str()) && pr > 0,
                    "publication receipt does not match the current Run"
                );
                ensure!(
                    current.stage == Stage::Publishing || current.pull_request == Some(pr),
                    "no matching publication is reserved"
                );
                current.stage = Stage::Published;
                current.pull_request = Some(pr);
                Ok(Reply::Run(Box::new(public_run(current))))
            }
        }
    }
}

fn public_run(run: &Attempt) -> Attempt {
    let mut run = run.clone();
    run.fence = None;
    run
}

fn lease(run: &Attempt) -> Result<Lease> {
    Ok(Lease {
        run: run.id.clone(),
        fence: run
            .fence
            .clone()
            .context("active Run is missing its fence")?,
        task: run.task.clone(),
        predecessor: run.predecessor.clone(),
        reverify: run.reverify,
    })
}

pub fn save(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

pub struct Store {
    path: PathBuf,
    state: State,
    _lease: std::fs::File,
}

impl Store {
    pub fn evidence_path(
        &self,
        id: &str,
        ticket: Option<&str>,
        proof: Option<(&str, &str)>,
    ) -> Result<PathBuf> {
        ident(id, 64)?;
        let run = self
            .state
            .attempts
            .iter()
            .find(|run| run.id == id)
            .context("unknown Run")?;
        ensure!(
            ticket.is_none_or(|ticket| run.task.ticket == ticket)
                && proof.is_none_or(|(worker, fence)| run.worker.as_deref() == Some(worker)
                    && run.fence.as_deref() == Some(fence)),
            "evidence scope does not match the Run"
        );
        ensure!(
            run.events
                .last()
                .is_some_and(|event| matches!(event.progress, Progress::Exit { .. })),
            "evidence requires a recorded terminal event"
        );
        Ok(self.path.with_extension("evidence").join(id))
    }

    pub fn open(path: PathBuf) -> Result<Self> {
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path.with_extension("lock"))?;
        lock.try_lock()
            .context("control state is already owned by a server")?;
        let mut state: State = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(error) => return Err(error.into()),
        };
        state.migrate_workers();
        Ok(Self {
            path,
            state,
            _lease: lock,
        })
    }

    pub fn apply(
        &mut self,
        operation: Operation,
        device: &str,
        now: u64,
        ttl: u64,
    ) -> Result<Reply> {
        let mut next = self.state.clone();
        let reply = next.apply(operation, device, now, ttl)?;
        next.normalize_queue();
        if next != self.state {
            save(&self.path, &next)?;
            self.state = next;
        }
        Ok(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task() -> Task {
        Task {
            ticket: "pilot".into(),
            name: "sample".into(),
            repository: "owner/repo".into(),
            spec: "1".repeat(40),
            target: "main".into(),
            baseline: "2".repeat(40),
            timeout: 120,
            max_attempts: 3,
        }
    }

    fn poll(store: &mut Store, now: u64) -> Result<Lease> {
        let Reply::Polled(Some(lease)) = store.apply(
            Operation::Poll {
                installation: "installation-1".into(),
                profile: Profile {
                    codex: "codex 1".into(),
                    claude: "claude 1".into(),
                },
            },
            "device",
            now,
            10,
        )?
        else {
            anyhow::bail!("no lease")
        };
        Ok(lease)
    }

    fn report(store: &mut Store, lease: &Lease, now: u64, event: Event) -> Result<Reply> {
        store.apply(
            Operation::Report {
                run: lease.run.clone(),
                fence: lease.fence.clone(),
                event,
            },
            "device",
            now,
            10,
        )
    }

    fn event(sequence: usize, progress: Progress) -> Event {
        Event {
            sequence,
            unix_seconds: sequence as u64,
            progress,
        }
    }

    fn start(store: &mut Store) -> Result<Lease> {
        store.apply(
            Operation::Dispatch {
                task: task(),
                revise: false,
            },
            "device",
            1,
            10,
        )?;
        poll(store, 2)
    }

    #[test]
    fn rolling_key_failure_preserves_the_key_and_keeps_the_peer_dispatching() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("control.json");
        let mut store = Store::open(path.clone())?;
        let profile = Profile {
            codex: "codex 1".into(),
            claude: "claude 1".into(),
        };
        let sync = |id: &str, inventory, receipt| Operation::KeySync {
            installation: format!("install-{id}"),
            profile: profile.clone(),
            repository: "owner/repo".into(),
            inventory,
            receipt,
        };
        let poll = |id: &str| Operation::Poll {
            installation: format!("install-{id}"),
            profile: profile.clone(),
        };
        let key = |number: char| aep::deploy_keys::Key {
            public: format!("ssh-ed25519 {}", number.to_string().repeat(64)),
            fingerprint: format!("SHA256:{}", number.to_string().repeat(43)),
        };
        let old = key('a');
        let first = Inventory {
            active: Some(old.clone()),
            ..Inventory::default()
        };
        let peer = Inventory {
            active: Some(key('b')),
            ..Inventory::default()
        };
        let new = key('c');
        let staged = Inventory {
            staged: Some(new.clone()),
            ..first.clone()
        };
        let request = |id: &str, change, rotation| Operation::KeyRequest {
            repository: "owner/repo".into(),
            worker: id.into(),
            installation: format!("install-{id}"),
            change,
            rotation,
        };
        let ack = |work: Request, error: Option<&str>| {
            Some(Receipt {
                id: work.id,
                error: error.map(str::to_owned),
            })
        };
        let work = |reply| match reply {
            Reply::KeyWork(Some(request)) => Ok(request),
            _ => anyhow::bail!("missing Deploy Key operation"),
        };
        store.apply(sync("a", first.clone(), None), "a", 1, 30)?;
        store.apply(sync("b", peer.clone(), None), "b", 1, 30)?;
        store.apply(
            Operation::KeyRotationStart {
                repository: "owner/repo".into(),
            },
            "operator",
            2,
            30,
        )?;
        let rotation = store
            .state
            .key_rotation
            .as_ref()
            .map(|plan| plan.id.clone());
        assert!(
            store
                .apply(
                    request("b", Change::Prepare, rotation.clone()),
                    "operator",
                    3,
                    30
                )
                .is_err()
        );
        assert!(
            store
                .apply(request("a", Change::Prepare, None), "operator", 3, 30)
                .is_err()
        );
        store.apply(
            request("a", Change::Prepare, rotation.clone()),
            "operator",
            3,
            30,
        )?;
        let prepare = work(store.apply(sync("a", first.clone(), None), "a", 4, 30)?)?;
        store.apply(sync("a", staged.clone(), ack(prepare, None)), "a", 5, 30)?;
        let activate = Change::Activate {
            fingerprint: new.fingerprint.clone(),
        };
        store.apply(
            request("a", activate.clone(), rotation.clone()),
            "operator",
            6,
            30,
        )?;
        let failed = work(store.apply(sync("a", staged.clone(), None), "a", 7, 30)?)?;
        store.apply(
            sync("a", staged.clone(), ack(failed, Some("read failed"))),
            "a",
            8,
            30,
        )?;
        drop(store);
        let mut store = Store::open(path)?;
        assert_eq!(store.state.deploy_keys["a"].inventory.active, first.active);
        assert_eq!(
            store
                .state
                .key_rotation
                .as_ref()
                .unwrap()
                .current()
                .unwrap()
                .worker,
            "a"
        );
        store.apply(
            Operation::Dispatch {
                task: task(),
                revise: false,
            },
            "operator",
            9,
            30,
        )?;
        assert!(matches!(
            store.apply(poll("a"), "a", 10, 30)?,
            Reply::Polled(None)
        ));
        assert!(matches!(
            store.apply(poll("b"), "b", 10, 30)?,
            Reply::Polled(Some(_))
        ));
        let retire = Change::Forget {
            fingerprint: old.fingerprint.clone(),
        };
        assert!(
            store
                .apply(
                    request("a", retire.clone(), rotation.clone()),
                    "operator",
                    11,
                    30
                )
                .is_err()
        );
        store.apply(request("a", activate, rotation.clone()), "operator", 11, 30)?;
        let retry = work(store.apply(sync("a", staged, None), "a", 12, 30)?)?;
        let switched = Inventory {
            active: Some(new.clone()),
            retiring: Some(old),
            staged: None,
        };
        store.apply(sync("a", switched.clone(), ack(retry, None)), "a", 13, 30)?;
        store.apply(request("a", retire, rotation.clone()), "operator", 14, 30)?;
        let forget = work(store.apply(sync("a", switched, None), "a", 15, 30)?)?;
        store.apply(
            sync(
                "a",
                Inventory {
                    active: Some(new.clone()),
                    ..Inventory::default()
                },
                ack(forget, None),
            ),
            "a",
            16,
            30,
        )?;
        let plan = store.state.key_rotation.as_ref().unwrap();
        assert_eq!(plan.current().unwrap().worker, "b");
        assert_eq!(
            plan.workers[0].replacement.as_deref(),
            Some(new.fingerprint.as_str())
        );
        assert_eq!(store.state.deploy_keys["b"].inventory, peer);
        assert!(
            store
                .apply(
                    request("a", Change::Prepare, rotation.clone()),
                    "operator",
                    17,
                    30
                )
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn deploy_key_requests_are_scoped_and_survive_restart_before_activation() -> Result<()> {
        let root = tempfile::tempdir()?;
        let file = root.path().join("control.json");
        let mut store = Store::open(file.clone())?;
        let profile = Profile {
            codex: "codex 1".into(),
            claude: "claude 1".into(),
        };
        let sync = |inventory, receipt| Operation::KeySync {
            installation: "installation-1".into(),
            profile: profile.clone(),
            repository: "owner/repo".into(),
            inventory,
            receipt,
        };
        let request = |repository: &str, change| Operation::KeyRequest {
            repository: repository.into(),
            worker: "device".into(),
            installation: "installation-1".into(),
            change,
            rotation: None,
        };
        let empty = Inventory::default();
        store.apply(sync(empty.clone(), None), "device", 1, 10)?;
        store.apply(
            Operation::Dispatch {
                task: task(),
                revise: false,
            },
            "device",
            2,
            10,
        )?;
        assert!(matches!(
            store.apply(
                Operation::Poll {
                    installation: "installation-1".into(),
                    profile: profile.clone()
                },
                "device",
                3,
                10
            )?,
            Reply::Polled(None)
        ));
        assert!(
            store
                .apply(request("owner/other", Change::Prepare), "device", 4, 10)
                .is_err()
        );
        store.apply(request("owner/repo", Change::Prepare), "device", 4, 10)?;
        drop(store);
        let mut store = Store::open(file)?;
        let Reply::KeyWork(Some(prepare)) = store.apply(sync(empty, None), "device", 5, 10)? else {
            anyhow::bail!("missing resumed key request");
        };
        assert_eq!(prepare.change, Change::Prepare);
        let key = aep::deploy_keys::Key {
            public: format!("ssh-ed25519 {}", "a".repeat(64)),
            fingerprint: format!("SHA256:{}", "b".repeat(43)),
        };
        let staged = Inventory {
            staged: Some(key.clone()),
            ..Inventory::default()
        };
        store.apply(
            sync(
                staged.clone(),
                Some(Receipt {
                    id: prepare.id,
                    error: None,
                }),
            ),
            "device",
            6,
            10,
        )?;
        let activate = Change::Activate {
            fingerprint: key.fingerprint.clone(),
        };
        store.apply(request("owner/repo", activate.clone()), "device", 6, 10)?;
        let Reply::KeyWork(Some(failed)) =
            store.apply(sync(staged.clone(), None), "device", 7, 10)?
        else {
            anyhow::bail!("missing activation");
        };
        store.apply(
            sync(
                staged.clone(),
                Some(Receipt {
                    id: failed.id,
                    error: Some("read failed".into()),
                }),
            ),
            "device",
            8,
            10,
        )?;
        let entry = &store.state.deploy_keys["device"];
        assert_eq!(entry.inventory, staged);
        assert_eq!(entry.verified_at, None);
        store.apply(request("owner/repo", activate), "device", 8, 10)?;
        let Reply::KeyWork(Some(retry)) = store.apply(sync(staged, None), "device", 9, 10)? else {
            anyhow::bail!("missing activation retry");
        };
        let active = Inventory {
            active: Some(key),
            ..Inventory::default()
        };
        store.apply(
            sync(
                active,
                Some(Receipt {
                    id: retry.id,
                    error: None,
                }),
            ),
            "device",
            10,
            10,
        )?;
        assert_eq!(store.state.deploy_keys["device"].verified_at, Some(10));
        assert_eq!(poll(&mut store, 11)?.task.repository, "owner/repo");
        Ok(())
    }

    #[test]
    fn leased_workers_progress_independently_and_reverify_where_the_candidate_lives() -> Result<()>
    {
        let root = tempfile::tempdir()?;
        let mut store = Store::open(root.path().join("control.json"))?;
        let profile = || Profile {
            codex: "codex 1".into(),
            claude: "claude 1".into(),
        };
        let poll = |id: &str| Operation::Poll {
            installation: format!("install-{id}"),
            profile: profile(),
        };
        let submit = |ticket: &str| Operation::Dispatch {
            task: Task {
                ticket: ticket.into(),
                ..task()
            },
            revise: false,
        };
        for ticket in ["a", "b"] {
            store.apply(submit(ticket), "operator", 1, 10)?;
        }
        let Reply::Polled(Some(a)) = store.apply(poll("one"), "one", 2, 10)? else {
            anyhow::bail!("Worker one has no Run");
        };
        let Reply::Polled(Some(b)) = store.apply(poll("two"), "two", 3, 10)? else {
            anyhow::bail!("Worker two cannot lease while Worker one runs");
        };
        assert_ne!(a.run, b.run);
        let candidate = "3".repeat(40);
        let exit_a = Operation::Report {
            run: a.run.clone(),
            fence: a.fence.clone(),
            event: event(
                1,
                Progress::Exit {
                    outcome: Outcome::Passed,
                    candidate: Some(candidate),
                },
            ),
        };
        let renew_a = Operation::Renew {
            run: a.run.clone(),
            fence: a.fence.clone(),
        };
        assert!(store.apply(renew_a, "two", 9, 10).is_err());
        assert!(store.apply(exit_a.clone(), "two", 10, 10).is_err());
        store.apply(
            Operation::Renew {
                run: b.run.clone(),
                fence: b.fence,
            },
            "two",
            11,
            10,
        )?;
        let Reply::Snapshot(s) = store.apply(Operation::Status, "operator", 12, 10)? else {
            anyhow::bail!("missing status");
        };
        assert_eq!(s.workers["one"].online, Some(false));
        assert_eq!(s.workers["two"].online, Some(true));
        assert_eq!(s.attempts[0].stage, Stage::Interrupted);
        assert_eq!(s.attempts[1].stage, Stage::Running);

        store.apply(exit_a, "one", 13, 10)?;
        assert!(
            store
                .evidence_path(&a.run, None, Some(("two", &a.fence)))
                .is_err()
        );
        assert!(
            store
                .evidence_path(&a.run, None, Some(("one", &a.fence)))
                .is_ok()
        );
        let Reply::Run(next) = store.apply(
            Operation::Retry {
                ticket: "a".into(),
                reverify: true,
            },
            "operator",
            14,
            10,
        )?
        else {
            anyhow::bail!("missing recovery");
        };
        assert_eq!(next.worker.as_deref(), Some("one"));
        let Reply::Polled(Some(existing)) = store.apply(poll("two"), "two", 14, 10)? else {
            anyhow::bail!("Worker two lost its Run");
        };
        assert_eq!(existing.run, b.run);
        let Reply::Polled(Some(retry)) = store.apply(poll("one"), "one", 15, 10)? else {
            anyhow::bail!("Worker one did not recover its Run");
        };
        assert_eq!(retry.run, next.id);
        assert_eq!(retry.predecessor.as_deref(), Some(a.run.as_str()));
        Ok(())
    }

    #[test]
    fn queue_controls_survive_restart_and_cancel_only_the_confirmed_run() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.json");
        let mut store = Store::open(path.clone())?;
        store.apply(Operation::QueuePause { paused: true }, "device", 1, 10)?;
        let mut ids = Vec::new();
        for ticket in ["first", "second"] {
            let mut task = task();
            task.ticket = ticket.into();
            let Reply::Run(run) = store.apply(
                Operation::Dispatch {
                    task,
                    revise: false,
                },
                "device",
                1,
                10,
            )?
            else {
                anyhow::bail!("missing dispatched Run")
            };
            ids.push(run.id);
        }
        let shift = Operation::QueueMove {
            run: ids[1].clone(),
            adjacent: ids[0].clone(),
            earlier: true,
        };
        store.apply(shift.clone(), "device", 2, 10)?;
        assert!(store.apply(shift, "device", 2, 10).is_err());
        drop(store);

        let mut store = Store::open(path)?;
        assert!(store.state.queue.paused);
        assert_eq!(store.state.queue.order, [ids[1].clone(), ids[0].clone()]);
        assert!(matches!(
            store.apply(
                Operation::Poll {
                    installation: "installation-1".into(),
                    profile: Profile {
                        codex: "codex 1".into(),
                        claude: "claude 1".into()
                    }
                },
                "device",
                3,
                10
            )?,
            Reply::Polled(None)
        ));
        store.apply(Operation::QueuePause { paused: false }, "device", 4, 10)?;
        let lease = poll(&mut store, 4)?;
        assert_eq!(lease.run, ids[1]);
        store.apply(Operation::QueuePause { paused: true }, "device", 4, 10)?;
        assert_eq!(poll(&mut store, 4)?.run, lease.run);
        let cancel = |id: &str, stage| Operation::Cancel {
            ticket: "second".into(),
            expected_run: Some(id.into()),
            expected_stage: Some(stage),
        };
        assert!(
            store
                .apply(cancel(&ids[0], Stage::Running), "device", 5, 10)
                .is_err()
        );
        assert!(
            store
                .apply(cancel(&ids[1], Stage::Queued), "device", 5, 10)
                .is_err()
        );
        assert!(matches!(
            store.apply(cancel(&ids[1], Stage::Running), "device", 5, 10)?,
            Reply::Run(run) if run.stage == Stage::CancelPending
        ));
        assert_eq!(poll(&mut store, 6)?.run, ids[1]);
        store.apply(
            Operation::Report {
                run: ids[1].clone(),
                fence: lease.fence,
                event: Event {
                    sequence: 1,
                    unix_seconds: 6,
                    progress: Progress::Exit {
                        outcome: Outcome::Passed,
                        candidate: Some("3".repeat(40)),
                    },
                },
            },
            "device",
            6,
            10,
        )?;
        assert_eq!(store.state.attempts[1].stage, Stage::Cancelled);
        assert!(matches!(
            store.apply(
                Operation::Poll {
                    installation: "installation-1".into(),
                    profile: Profile {
                        codex: "codex 1".into(),
                        claude: "claude 1".into()
                    }
                },
                "device",
                7,
                10
            )?,
            Reply::Polled(None)
        ));
        store.apply(Operation::QueuePause { paused: false }, "device", 8, 10)?;
        assert_eq!(poll(&mut store, 8)?.run, ids[0]);
        Ok(())
    }

    #[test]
    fn expired_lease_can_replay_metadata_but_cannot_publish() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("control.json");
        let mut store = Store::open(path.clone())?;
        let first = start(&mut store)?;
        assert_eq!(poll(&mut store, 3)?, first);
        assert!(
            store
                .apply(
                    Operation::Poll {
                        installation: "different-clone".into(),
                        profile: Profile {
                            codex: "codex 1".into(),
                            claude: "claude 1".into()
                        },
                    },
                    "device",
                    3,
                    10
                )
                .is_err()
        );
        let started = event(
            1,
            Progress::Phase {
                phase: Phase::Checking,
                candidate: Some("3".repeat(40)),
            },
        );
        assert!(matches!(
            report(&mut store, &first, 3, started.clone())?,
            Reply::Ack(1)
        ));
        drop(store);
        let mut store = Store::open(path)?;
        assert!(matches!(
            report(&mut store, &first, 15, started)?,
            Reply::Ack(1)
        ));
        assert!(matches!(
            store.apply(
                Operation::Renew {
                    run: first.run.clone(),
                    fence: first.fence.clone()
                },
                "device",
                15,
                10
            )?,
            Reply::Renewed {
                stale: true,
                cancel: false
            }
        ));
        assert!(matches!(
            report(
                &mut store,
                &first,
                15,
                event(
                    2,
                    Progress::Exit {
                        outcome: Outcome::Passed,
                        candidate: Some("3".repeat(40)),
                    }
                )
            )?,
            Reply::Ack(2)
        ));
        assert_eq!(store.state.attempts[0].stage, Stage::Interrupted);
        assert!(
            store
                .apply(
                    Operation::BeginPublication {
                        ticket: "pilot".into(),
                        run: first.run.clone(),
                        spec: task().spec,
                        candidate: "3".repeat(40),
                        report_sha256: "5".repeat(64),
                    },
                    "device",
                    16,
                    10
                )
                .is_err()
        );
        store.apply(
            Operation::Retry {
                ticket: "pilot".into(),
                reverify: true,
            },
            "device",
            16,
            10,
        )?;
        let retry = poll(&mut store, 17)?;
        assert_ne!(retry.run, first.run);
        assert!(retry.reverify);
        assert_eq!(retry.predecessor, Some(first.run));
        Ok(())
    }

    #[test]
    fn cancellation_after_expiry_waits_for_executor_and_discards_success() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut store = Store::open(directory.path().join("control.json"))?;
        let lease = start(&mut store)?;
        assert!(
            matches!(store.apply(Operation::Cancel { ticket: "pilot".into(), expected_run: None, expected_stage: None }, "device", 14, 10)?, Reply::Run(run) if run.stage == Stage::CancelPending)
        );
        assert!(matches!(
            store.apply(
                Operation::Renew {
                    run: lease.run.clone(),
                    fence: lease.fence.clone()
                },
                "device",
                15,
                10
            )?,
            Reply::Renewed {
                stale: false,
                cancel: true
            }
        ));
        report(
            &mut store,
            &lease,
            16,
            event(
                1,
                Progress::Exit {
                    outcome: Outcome::Passed,
                    candidate: Some("3".repeat(40)),
                },
            ),
        )?;
        assert_eq!(store.state.attempts[0].stage, Stage::Cancelled);
        Ok(())
    }

    #[test]
    fn revision_fences_old_human_approval_and_history_is_preserved() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut store = Store::open(directory.path().join("control.json"))?;
        let lease = start(&mut store)?;
        report(
            &mut store,
            &lease,
            4,
            event(
                1,
                Progress::Exit {
                    outcome: Outcome::Passed,
                    candidate: Some("3".repeat(40)),
                },
            ),
        )?;
        store.apply(
            Operation::Approve {
                ticket: "pilot".into(),
                run: lease.run.clone(),
                actor: "maintainer".into(),
                spec: task().spec,
                candidate: "3".repeat(40),
                report_sha256: "5".repeat(64),
            },
            "device",
            4,
            10,
        )?;
        let revised = Task {
            name: "sample-v2".into(),
            spec: "4".repeat(40),
            ..task()
        };
        store.apply(
            Operation::Dispatch {
                task: revised,
                revise: true,
            },
            "device",
            5,
            10,
        )?;
        assert!(
            store
                .apply(
                    Operation::BeginPublication {
                        ticket: "pilot".into(),
                        run: lease.run,
                        spec: task().spec,
                        candidate: "3".repeat(40),
                        report_sha256: "5".repeat(64),
                    },
                    "device",
                    5,
                    10
                )
                .is_err()
        );
        assert_eq!(store.state.attempts[0].stage, Stage::Superseded);
        assert_eq!(
            store.state.attempts[0].approval.as_ref().unwrap().actor,
            "maintainer"
        );
        Ok(())
    }
}
