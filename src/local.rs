mod author;

use crate::{
    Execution,
    git::{Repo, commit, git, ident},
    run,
};
use aep::{
    cancel::Cancellation,
    studio::{Artifact, preview},
    workspace::validate_task_target,
};
use anyhow::{Context, Result, bail, ensure};
use chrono::Utc;
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, DirBuilder, File, OpenOptions, TryLockError},
    io::ErrorKind,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    process::ExitCode,
};

#[derive(Subcommand)]
pub(crate) enum Action {
    /// Propose a task and its checks for a committed Spec and a local target.
    Plan {
        #[arg(long)]
        ticket: String,
        #[arg(long)]
        task: String,
        #[arg(long)]
        spec: String,
        #[arg(long, default_value = "main")]
        target: String,
        #[arg(long, default_value_t = 600, value_parser = clap::value_parser!(u64).range(1..=3600))]
        timeout: u64,
        #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u64).range(1..=8))]
        attempts: u64,
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        max_tokens: Option<u64>,
        /// Cap summed provider-reported USD, expressed in millionths of a dollar.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        max_cost_micros: Option<u64>,
        #[arg(long)]
        revise: bool,
        /// Pin a Codex model supported by the operator's account.
        #[arg(long)]
        codex_model: Option<String>,
    },
    /// Write or revise a local Spec without changing the checkout or index.
    Author(author::Request),
    /// List local target branches and their current commits.
    Targets,
    /// Approve this exact task plan before starting an agent.
    ApprovePlan {
        #[command(flatten)]
        plan: PlanScope,
    },
    /// Start implementation or retry the latest unfinished delivery.
    Run {
        #[arg(long)]
        ticket: String,
        /// Explicitly start a new attempt after a passing Run.
        #[arg(long)]
        rerun: bool,
    },
    Cancel {
        #[arg(long)]
        ticket: String,
        #[arg(long)]
        run: String,
    },
    List,
    /// Summarize evidence, elapsed time, human effort and observed model usage.
    Report,
    Status {
        #[arg(long)]
        ticket: String,
    },
    /// Read the planned checks and local Diff, test log and review.
    Inspect {
        #[arg(long)]
        ticket: String,
        #[arg(long)]
        version: Option<u64>,
        #[arg(long)]
        run: Option<String>,
    },
    /// Approve the exact Run, Spec, baseline, candidate and evidence.
    ApproveDelivery {
        #[command(flatten)]
        delivery: DeliveryScope,
        /// Digest printed by inspect for these exact checks and review.
        #[arg(long)]
        report_sha256: String,
        /// Operator time including specification, interruption and review.
        #[arg(long)]
        human_minutes: Option<u64>,
        #[arg(long)]
        baseline_minutes: Option<u64>,
    },
    /// Fast-forward the clean checked-out target using a recorded approval.
    Merge {
        #[command(flatten)]
        delivery: DeliveryScope,
    },
}

#[derive(Args)]
pub(crate) struct PlanScope {
    #[arg(long)]
    ticket: String,
    #[arg(long)]
    version: u64,
    #[arg(long)]
    spec: String,
    #[arg(long)]
    baseline: String,
}

#[derive(Args)]
pub(crate) struct DeliveryScope {
    #[command(flatten)]
    plan: PlanScope,
    #[arg(long)]
    run: String,
    #[arg(long)]
    candidate: String,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
struct Pin {
    task: String,
    spec: String,
    target: String,
    baseline: String,
    timeout: u64,
    max_attempts: u64,
    max_tokens: Option<u64>,
    max_cost_micros: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    codex_model: Option<String>,
}

impl Pin {
    fn read(
        repo: &Repo,
        task: String,
        spec: &str,
        target: String,
        timeout: u64,
        attempts: u64,
        limits: (Option<u64>, Option<u64>, Option<String>),
    ) -> Result<Self> {
        ident(&task, 63)?;
        let baseline = target_commit(repo, &target)?;
        let spec = commit(&repo.root, spec)?;
        validate_task_target(&repo.root, &baseline, &spec, &task)?;
        let (_, manifest) = repo.task(&spec, &task)?;
        ensure!(
            manifest
                .title
                .as_deref()
                .is_some_and(|title| !title.trim().is_empty()),
            "the implementation task needs a title"
        );
        ensure!(
            limits
                .2
                .as_ref()
                .is_none_or(|model| !model.trim().is_empty()),
            "empty Codex model"
        );
        Ok(Self {
            task,
            spec,
            target,
            baseline,
            timeout,
            max_attempts: attempts,
            max_tokens: limits.0,
            max_cost_micros: limits.1,
            codex_model: limits.2,
        })
    }

    fn check_target(&self, repo: &Repo) -> Result<()> {
        ensure!(
            target_commit(repo, &self.target)? == self.baseline,
            "target branch moved; revise the plan and verify against its new baseline"
        );
        Ok(())
    }
}

#[derive(Clone, Deserialize, Serialize)]
struct Stamp {
    actor: String,
    at: i64,
}

impl Stamp {
    fn new(repo: &Repo) -> Result<Self> {
        let name = git(&repo.root, &["config", "user.name"])?;
        let email = git(&repo.root, &["config", "user.email"])?;
        ensure!(
            !name.trim().is_empty() && !email.trim().is_empty(),
            "configure the Git approver identity"
        );
        Ok(Self {
            actor: format!("{} <{}>", name.trim(), email.trim()),
            at: Utc::now().timestamp(),
        })
    }
}

#[derive(Deserialize, Serialize)]
struct Approval {
    approval: Stamp,
    run: String,
    candidate: String,
    sha256: String,
    human_minutes: Option<u64>,
    baseline_minutes: Option<u64>,
}

#[derive(Deserialize, Serialize)]
struct Plan {
    #[serde(flatten)]
    pin: Pin,
    version: u64,
    created_at: i64,
    plan_approval: Option<Stamp>,
    attempts: Vec<String>,
    delivery: Option<Approval>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    previous_deliveries: Vec<Approval>,
    merging: Option<Stamp>,
    merged_at: Option<i64>,
}

impl Plan {
    fn propose(pin: Pin, version: u64) -> Self {
        Self {
            pin,
            version,
            created_at: Utc::now().timestamp(),
            plan_approval: None,
            attempts: Vec::new(),
            delivery: None,
            previous_deliveries: Vec::new(),
            merging: None,
            merged_at: None,
        }
    }

    fn require_scope(&self, scope: &PlanScope) -> Result<()> {
        ensure!(
            self.version == scope.version
                && self.pin.spec == scope.spec
                && self.pin.baseline == scope.baseline,
            "plan version, Spec or baseline changed; inspect and confirm the current plan"
        );
        Ok(())
    }

    fn records(&self, repo: &Repo) -> Result<Vec<Value>> {
        self.attempts.iter().map(|id| saved_run(repo, id)).collect()
    }
}

#[derive(Deserialize, Serialize)]
struct Ticket {
    ticket: String,
    current: Plan,
    history: Vec<Plan>,
}

fn target_commit(repo: &Repo, target: &str) -> Result<String> {
    let reference = format!("refs/heads/{target}");
    ensure!(!target.starts_with('-'), "invalid target branch");
    git(&repo.root, &["check-ref-format", &reference])?;
    commit(&repo.root, &reference)
}

fn dir(repo: &Repo) -> PathBuf {
    repo.data.join("local-tasks")
}

fn path(repo: &Repo, ticket: &str) -> Result<PathBuf> {
    ident(ticket, 63)?;
    Ok(dir(repo).join(format!("{ticket}.json")))
}

fn load(repo: &Repo, ticket: &str) -> Result<Ticket> {
    let record: Ticket = serde_json::from_reader(File::open(path(repo, ticket)?)?)?;
    ensure!(
        record.ticket == ticket,
        "stored task ID differs from its file"
    );
    Ok(record)
}

pub(crate) fn workflow_memory_scope(
    repo: &Repo,
    ticket: &str,
    head: &str,
) -> Result<Option<aep::memory::Key>> {
    Ok(existing(repo, ticket)?
        .map(|t| aep::memory::Key::new(&t.current.pin.task, &t.current.pin.spec, head)))
}
fn existing(repo: &Repo, ticket: &str) -> Result<Option<Ticket>> {
    path(repo, ticket)?
        .try_exists()?
        .then(|| load(repo, ticket))
        .transpose()
}

fn propose(
    repo: &Repo,
    ticket: String,
    pin: Pin,
    prior: Option<Ticket>,
    revise: bool,
) -> Result<Ticket> {
    pin.check_target(repo)?;
    let record = if let Some(mut saved) = prior {
        if saved.current.pin != pin {
            ensure!(
                revise,
                "ticket already pins a different plan; explicitly revise it"
            );
            ensure!(
                saved.current.merging.is_none() || saved.current.merged_at.is_some(),
                "reconcile the pending merge before revising"
            );
            let next = Plan::propose(pin, saved.current.version + 1);
            saved
                .history
                .push(std::mem::replace(&mut saved.current, next));
        }
        saved
    } else {
        Ticket {
            ticket,
            current: Plan::propose(pin, 1),
            history: Vec::new(),
        }
    };
    let pin_spec = |spec: &str| {
        git(
            &repo.root,
            &["update-ref", &format!("refs/aep/local-specs/{spec}"), spec],
        )
    };
    for version in &record.history {
        if commit(&repo.root, &version.pin.spec).is_ok() {
            pin_spec(&version.pin.spec)?;
        }
    }
    pin_spec(&record.current.pin.spec)?;
    save(repo, &record)?;
    Ok(record)
}

fn lock(repo: &Repo, ticket: &str) -> Result<File> {
    ident(ticket, 63)?;
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir(repo))?;
    try_lock(&dir(repo).join(format!("{ticket}.lock")))
}

fn try_lock(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => bail!("task is already being updated or executed"),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}

fn save(repo: &Repo, ticket: &Ticket) -> Result<()> {
    let temporary = tempfile::NamedTempFile::new_in(dir(repo))?;
    serde_json::to_writer_pretty(temporary.as_file(), ticket)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path(repo, &ticket.ticket)?)?;
    Ok(())
}

fn saved_run(repo: &Repo, id: &str) -> Result<Value> {
    ident(id, 64)?;
    if !repo.data.join("runs").join(id).exists() {
        return Ok(json!({"run_id":id, "status":"unstarted"}));
    }
    run::status(repo, id)
}

#[derive(Default, Serialize)]
struct Usage {
    tokens: u64,
    reported_cost_micros: u64,
    unmetered_calls: usize,
    unpriced_calls: usize,
}

fn usage(records: &[Value]) -> Usage {
    let mut total = Usage::default();
    for record in records.iter().filter(|r| r["started_at"].is_string()) {
        for role in ["implementer", "reviewer"] {
            let entry = &record["usage"][role];
            let launched = if role == "implementer" {
                record["implementation_source"] == "codex"
            } else {
                !entry.is_null()
                    || record["phase"] == "reviewing"
                    || record["review_status"] != "pending"
            };
            if !launched {
                continue;
            }
            total.unmetered_calls +=
                usize::from(!entry["input_tokens"].is_u64() || !entry["output_tokens"].is_u64());
            for field in [
                "input_tokens",
                "output_tokens",
                "cache_creation_input_tokens",
                "cache_read_input_tokens",
            ] {
                total.tokens = total
                    .tokens
                    .saturating_add(entry[field].as_u64().unwrap_or(0));
            }
            if let Some(cost) = entry["reported_cost_usd"]
                .as_f64()
                .filter(|value| *value >= 0.0)
            {
                total.reported_cost_micros = total
                    .reported_cost_micros
                    .saturating_add((cost * 1_000_000.0).ceil() as u64);
            } else {
                total.unpriced_calls += 1;
            }
        }
    }
    total
}

fn blocked(pin: &Pin, total: &Usage, for_retry: bool) -> Option<&'static str> {
    if let Some(limit) = pin.max_tokens {
        if total.unmetered_calls > 0 {
            return Some("a model call has no complete token receipt");
        }
        if total.tokens > limit || (for_retry && total.tokens == limit) {
            return Some("token budget exhausted");
        }
    }
    if let Some(limit) = pin.max_cost_micros {
        if total.unpriced_calls > 0 {
            return Some("a model call has no reported cost; review the provider receipt");
        }
        if total.reported_cost_micros > limit || (for_retry && total.reported_cost_micros == limit)
        {
            return Some("reported cost budget exhausted");
        }
    }
    None
}

pub(crate) struct Budget {
    pin: Pin,
    prior: Usage,
}

impl Budget {
    pub(crate) fn check(&self, record: &mut Value, next_call: bool) -> Result<()> {
        let current = usage(std::slice::from_ref(record));
        let total = Usage {
            tokens: self.prior.tokens.saturating_add(current.tokens),
            reported_cost_micros: self
                .prior
                .reported_cost_micros
                .saturating_add(current.reported_cost_micros),
            unmetered_calls: self
                .prior
                .unmetered_calls
                .saturating_add(current.unmetered_calls),
            unpriced_calls: self
                .prior
                .unpriced_calls
                .saturating_add(current.unpriced_calls),
        };
        if let Some(reason) = blocked(&self.pin, &total, next_call) {
            record["budget_blocked"] = json!(reason);
            bail!("{reason}");
        }
        Ok(())
    }
}

#[derive(Default)]
struct Inspection<'a> {
    version: Option<u64>,
    run: Option<&'a str>,
}

fn view(repo: &Repo, ticket: &Ticket, inspection: Option<Inspection<'_>>) -> Result<Value> {
    let requested_version = inspection.as_ref().and_then(|scope| scope.version);
    let plan = match requested_version {
        Some(version) if version != ticket.current.version => ticket
            .history
            .iter()
            .find(|plan| plan.version == version)
            .context("unknown task plan version")?,
        _ => &ticket.current,
    };
    let records = plan.records(repo)?;
    let mut all = Vec::new();
    for version in ticket
        .history
        .iter()
        .chain(std::iter::once(&ticket.current))
    {
        if version.version == plan.version {
            all.extend(records.iter().cloned());
        } else {
            all.extend(version.records(repo)?);
        }
    }
    let current_usage = usage(&records);
    let requested_run = inspection.as_ref().and_then(|scope| scope.run);
    let historical = plan.version != ticket.current.version
        || requested_run.is_some_and(|run| plan.attempts.last().map(String::as_str) != Some(run));
    let last = match requested_run {
        Some(run) => records
            .iter()
            .find(|record| record["run_id"] == run)
            .cloned()
            .context("Run does not belong to the selected task version")?,
        None => records.last().cloned().unwrap_or_default(),
    };
    let branch_block = last["budget_blocked"]
        .as_str()
        .or_else(|| blocked(&plan.pin, &current_usage, false));
    let phase = if historical {
        "history"
    } else if plan.merged_at.is_some() {
        "merged"
    } else if plan.merging.is_some() {
        "merge_pending"
    } else if plan.plan_approval.is_none() {
        "plan_pending"
    } else if plan.delivery.is_some() {
        "approved"
    } else if matches!(last["status"].as_str(), Some("running" | "initializing")) {
        "running"
    } else if branch_block.is_some() {
        "blocked"
    } else if plan.attempts.is_empty() {
        "ready"
    } else if last["status"] == "awaiting_human_review" {
        "review_pending"
    } else if plan.attempts.len() as u64 >= plan.pin.max_attempts && last["status"] != "unstarted" {
        "blocked"
    } else {
        "retry"
    };
    let mut result = json!({
        "ticket":ticket.ticket,
        "current":plan,
        "history":ticket.history,
        "historical":historical,
        "latest_version":ticket.current.version,
        "phase":phase,
        "reason":branch_block.or_else(|| (phase=="blocked").then_some("attempt budget exhausted")),
        "run":last,
        "runs":records.iter().map(|r| json!({
            "id":r["run_id"], "status":r["status"], "candidate":r["candidate_sha"],
            "error":r["error"], "usage":r["usage"], "feedback_run":r["request"]["feedback_run"],
        })).collect::<Vec<_>>(),
        "usage":current_usage,
        "total_usage":usage(&all),
        "elapsed_seconds":plan.delivery.as_ref().map(|approval| approval.approval.at.saturating_sub(plan.created_at)),
    });
    if inspection.is_some() {
        let (spec, manifest) = repo.task(&plan.pin.spec, &plan.pin.task)?;
        result["proposal"] =
            json!({"spec":spec,"manifest":manifest,"implementer":"codex","reviewer":"claude"});
        if let Some(id) = last["run_id"].as_str() {
            for (field, artifact) in [
                ("implementation", Artifact::Implementation),
                ("checks", Artifact::Checks),
                ("review", Artifact::Review),
            ] {
                if let Some(item) = preview(&repo.data, id, artifact)? {
                    result[field] = json!({"text":item.text,"truncated":item.truncated});
                }
            }
            if let Some(candidate) = last["candidate_sha"].as_str() {
                let diff = git(
                    &repo.root,
                    &[
                        "diff",
                        "--no-ext-diff",
                        "--no-textconv",
                        &plan.pin.baseline,
                        candidate,
                        "--",
                    ],
                )?;
                let mut end = diff.len().min(96 * 1024);
                while !diff.is_char_boundary(end) {
                    end -= 1;
                }
                result["diff"] = json!({"text":&diff[..end],"truncated":end<diff.len()});
            }
            if !historical
                && last["status"] == "awaiting_human_review"
                && last["review_status"] == "findings_recorded"
            {
                result["report_sha256"] = json!(run::approval_digest(&repo.data, id, &last)?);
            }
        }
    }
    Ok(result)
}

fn list(repo: &Repo) -> Result<Vec<Value>> {
    let entries = match fs::read_dir(dir(repo)) {
        Ok(dir) => dir,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err.into()),
    };
    let mut tasks = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_file()
            && path.extension().is_some_and(|ext| ext == "json")
            && let Some(id) = path.file_stem().and_then(|name| name.to_str())
        {
            let record = load(repo, id)?;
            let row = view(repo, &record, None)?;
            tasks.push(json!({"ticket":id,"task":record.current.pin.task,"target":record.current.pin.target,"phase":row["phase"]}));
        }
    }
    tasks.sort_by(|a, b| a["ticket"].as_str().cmp(&b["ticket"].as_str()));
    Ok(tasks)
}

fn start(repo: &Repo, id: &str, rerun: bool) -> Result<ExitCode> {
    let _guard = lock(repo, id)?;
    let mut ticket = load(repo, id)?;
    let plan = &mut ticket.current;
    ensure!(
        plan.plan_approval.is_some(),
        "approve the task plan before running"
    );
    ensure!(
        plan.merging.is_none() && plan.merged_at.is_none(),
        "this version was submitted for merge"
    );
    plan.pin.check_target(repo)?;
    let records = plan.records(repo)?;
    let prior_usage = usage(&records);
    if let Some(reason) = blocked(&plan.pin, &prior_usage, true) {
        bail!("{reason}");
    }
    let last = records.last();
    let state = last.and_then(|r| r["status"].as_str()).unwrap_or("");
    ensure!(
        !matches!(state, "running" | "initializing"),
        "the latest Run is still executing"
    );
    ensure!(
        state != "awaiting_human_review" || rerun,
        "this Run passed; use --rerun to invalidate its delivery approval"
    );
    let prior = if state == "unstarted" {
        records.iter().rev().nth(1)
    } else {
        last
    };
    let id = if state == "unstarted" {
        plan.attempts
            .last()
            .context("missing reserved Run")?
            .clone()
    } else {
        ensure!(
            (plan.attempts.len() as u64) < plan.pin.max_attempts,
            "attempt budget exhausted"
        );
        let id = uuid::Uuid::new_v4().simple().to_string();
        plan.attempts.push(id.clone());
        id
    };
    let previous = prior.filter(|r| {
        matches!(
            r["status"].as_str(),
            Some("failed" | "cancelled" | "needs_reconciliation")
        ) && r["spec_sha"] == plan.pin.spec
    });
    let candidate = previous
        .and_then(|r| r["candidate_sha"].as_str())
        .map(str::to_owned);
    let feedback = previous
        .and_then(|r| r["run_id"].as_str())
        .map(str::to_owned);
    let pin = plan.pin.clone();
    if let Some(approval) = plan.delivery.take() {
        plan.previous_deliveries.push(approval);
    }
    save(repo, &ticket)?;
    let result = run::execute(
        repo,
        &Execution {
            task: pin.task.clone(),
            candidate,
            run_id: Some(id.clone()),
            shared_memory: true,
            require_memory: None,
            reviewer: "claude".into(),
            timeout: pin.timeout,
            whole_attempt: true,
            feedback_run: feedback,
            budget: Some(Budget {
                pin: pin.clone(),
                prior: prior_usage,
            }),
        },
        run::Base::Local(&pin.spec),
        Some("codex"),
        pin.codex_model.as_deref(),
        None,
    )?;
    if let Some(reason) = blocked(&pin, &usage(&ticket.current.records(repo)?), false) {
        bail!("{reason}");
    }
    Ok(result)
}

fn checked_delivery(repo: &Repo, plan: &Plan, scope: &DeliveryScope) -> Result<String> {
    plan.require_scope(&scope.plan)?;
    ensure!(
        plan.plan_approval.is_some() && plan.attempts.last() == Some(&scope.run),
        "delivery must use the current Run of an approved plan"
    );
    if let Some(reason) = blocked(&plan.pin, &usage(&plan.records(repo)?), false) {
        bail!("{reason}");
    }
    let r = saved_run(repo, &scope.run)?;
    ensure!(
        r["status"] == "awaiting_human_review"
            && r["check_exit_code"] == 0
            && r["reviewer"] == "claude"
            && r["review_status"] == "findings_recorded"
            && r["blocking_findings"].as_array().is_none_or(Vec::is_empty)
            && r["task"] == plan.pin.task
            && r["spec_sha"] == plan.pin.spec
            && r["candidate_sha"] == scope.candidate
            && r["acceptance"]
                .as_array()
                .is_some_and(|steps| !steps.is_empty()
                    && steps
                        .iter()
                        .all(|step| step["status"] == "passed" && step["exit_code"] == 0)),
        "the current candidate needs passing checks and independent review"
    );
    let retained = commit(&repo.root, &format!("refs/aep/runs/{}", scope.run))?;
    ensure!(retained == scope.candidate, "candidate reference changed");
    let (_, task) = repo.task(&plan.pin.spec, &plan.pin.task)?;
    repo.changes(&plan.pin.spec, &retained, &task.allowed_paths)?;
    run::approval_digest(&repo.data, &scope.run, &r)
}

fn merge(repo: &Repo, mut ticket: Ticket, scope: &DeliveryScope) -> Result<Ticket> {
    let _integration = try_lock(&repo.data.join("local-delivery.lock"))?;
    let plan = &mut ticket.current;
    let digest = checked_delivery(repo, plan, scope)?;
    let approval = plan
        .delivery
        .as_ref()
        .context("approve the delivery before merging")?;
    ensure!(
        approval.run == scope.run
            && approval.candidate == scope.candidate
            && approval.sha256 == digest,
        "the approved candidate or evidence changed"
    );
    if plan.merged_at.is_some() {
        return Ok(ticket);
    }
    let target = plan.pin.target.clone();
    let baseline = plan.pin.baseline.clone();
    let actual = target_commit(repo, &target)?;
    let recovered = plan.merging.is_some() && actual == scope.candidate;
    if !recovered {
        ensure!(
            actual == baseline,
            "target branch moved; revise and reapprove the task"
        );
        ensure!(
            git(&repo.root, &["symbolic-ref", "-q", "HEAD"])?.trim_end()
                == format!("refs/heads/{target}"),
            "check out the approved target branch before merging"
        );
        ensure!(
            git(
                &repo.root,
                &["status", "--porcelain", "--untracked-files=all"]
            )?
            .is_empty(),
            "target worktree must be clean before merging"
        );
        if plan.merging.is_none() {
            plan.merging = Some(Stamp::new(repo)?);
            save(repo, &ticket)?;
        }
        git(&repo.root, &["merge", "--ff-only", &scope.candidate])?;
        ensure!(
            target_commit(repo, &target)? == scope.candidate,
            "merge result differs from the approved candidate"
        );
    }
    ticket.current.merged_at = Some(Utc::now().timestamp());
    save(repo, &ticket)?;
    Ok(ticket)
}

fn print(value: &impl Serialize) -> Result<ExitCode> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn execute(repo: &Repo, action: Action) -> Result<ExitCode> {
    let record = match action {
        Action::List => return print(&list(repo)?),
        Action::Targets => return print(&author::targets(repo)?),
        Action::Author(request) => author::create(repo, request)?,
        Action::Report => {
            let mut tasks = Vec::new();
            for row in list(repo)? {
                let id = row["ticket"].as_str().context("missing ticket")?;
                let ticket = load(repo, id)?;
                let status = view(repo, &ticket, None)?;
                let mut versions = Vec::new();
                for plan in ticket
                    .history
                    .iter()
                    .chain(std::iter::once(&ticket.current))
                {
                    let approval = plan.delivery.as_ref();
                    versions.push(json!({
                        "version":plan.version,"spec":plan.pin.spec,"baseline":plan.pin.baseline,
                        "attempts":plan.attempts.len(),"finished":plan.merged_at.is_some(),
                        "elapsed_seconds":approval.map(|entry|entry.approval.at.saturating_sub(plan.created_at)),
                        "human_minutes":approval.and_then(|entry|entry.human_minutes),
                        "baseline_minutes":approval.and_then(|entry|entry.baseline_minutes),
                        "usage":usage(&plan.records(repo)?),
                    }));
                }
                tasks.push(json!({"ticket":id,"phase":status["phase"],"versions":versions,"usage":status["total_usage"]}));
            }
            return print(&json!({"tasks":tasks}));
        }
        Action::Status { ticket } => return print(&view(repo, &load(repo, &ticket)?, None)?),
        Action::Inspect {
            ticket,
            version,
            run,
        } => {
            return print(&view(
                repo,
                &load(repo, &ticket)?,
                Some(Inspection {
                    version,
                    run: run.as_deref(),
                }),
            )?);
        }
        Action::Run { ticket, rerun } => return start(repo, &ticket, rerun),
        Action::Cancel { ticket, run } => {
            let current = load(repo, &ticket)?.current;
            ensure!(
                current.attempts.last() == Some(&run),
                "cancel must match the current Run"
            );
            ensure!(
                saved_run(repo, &run)?["status"] == "running",
                "this Run is not executing"
            );
            ensure!(
                Cancellation::new(&repo.data, &run)?.request()?,
                "the Run already stopped"
            );
            return print(&json!({"cancel_requested":run}));
        }
        Action::Plan {
            ticket,
            task,
            spec,
            target,
            timeout,
            attempts,
            max_tokens,
            max_cost_micros,
            revise,
            codex_model,
        } => {
            let _guard = lock(repo, &ticket)?;
            let pin = Pin::read(
                repo,
                task,
                &spec,
                target,
                timeout,
                attempts,
                (max_tokens, max_cost_micros, codex_model),
            )?;
            let prior = existing(repo, &ticket)?;
            propose(repo, ticket, pin, prior, revise)?
        }
        Action::ApprovePlan { plan } => {
            let _guard = lock(repo, &plan.ticket)?;
            let mut record = load(repo, &plan.ticket)?;
            record.current.require_scope(&plan)?;
            record.current.pin.check_target(repo)?;
            if record.current.plan_approval.is_none() {
                record.current.plan_approval = Some(Stamp::new(repo)?);
                save(repo, &record)?;
            }
            record
        }
        Action::ApproveDelivery {
            delivery,
            report_sha256,
            human_minutes,
            baseline_minutes,
        } => {
            let _guard = lock(repo, &delivery.plan.ticket)?;
            let mut record = load(repo, &delivery.plan.ticket)?;
            record.current.pin.check_target(repo)?;
            let digest = checked_delivery(repo, &record.current, &delivery)?;
            ensure!(
                digest == report_sha256,
                "reviewed evidence changed; inspect again before approving"
            );
            ensure!(
                record.current.merging.is_none(),
                "merge intent already recorded"
            );
            let replacement = Approval {
                approval: Stamp::new(repo)?,
                run: delivery.run,
                candidate: delivery.candidate,
                sha256: digest,
                human_minutes,
                baseline_minutes,
            };
            if !record.current.delivery.as_ref().is_some_and(|prior| {
                prior.run == replacement.run
                    && prior.candidate == replacement.candidate
                    && prior.sha256 == replacement.sha256
                    && prior.human_minutes == replacement.human_minutes
                    && prior.baseline_minutes == replacement.baseline_minutes
            }) {
                if let Some(prior) = record.current.delivery.replace(replacement) {
                    record.current.previous_deliveries.push(prior);
                }
                save(repo, &record)?;
            }
            record
        }
        Action::Merge { delivery } => {
            let _guard = lock(repo, &delivery.plan.ticket)?;
            merge(repo, load(repo, &delivery.plan.ticket)?, &delivery)?
        }
    };
    print(&view(repo, &record, None)?)
}
