//! Whether a stage run continues an existing CLI session or starts a new one (R2, R3).
//!
//! One pure function, [`decide`], is shared by the runner and the Studio preview so the
//! preview always matches what the runner does (the runner only adds the E4 fallback when
//! the CLI cannot find the session after all).
use crate::workflow::{Client, Request, Role, Sandbox};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Default resume limit: a session whose last call already read more than this is not
/// continued; a new session with a hand-off digest is cheaper.
pub const DEFAULT_LIMIT: u64 = 150_000;
/// Below this almost every run would start fresh and reuse would mean nothing.
pub const MIN_LIMIT: u64 = 20_000;

/// The limit a request runs with: the project's setting, or the default for older requests.
pub fn limit(configured: Option<u64>) -> u64 {
    configured.unwrap_or(DEFAULT_LIMIT).max(MIN_LIMIT)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The operator continues this stage's conversation.
    Continue,
    /// A stage reuses the previous stage's session.
    Adjacent,
}

/// What a stage may do, as the runner derives it from the stage role.
pub fn permission(role: Role) -> &'static str {
    match role {
        Role::Build | Role::Plan => "write",
        Role::Test | Role::Accept => "gate",
        Role::Deliver => "read",
    }
}

/// The comparable properties of one run.
#[derive(Clone, Debug, PartialEq)]
pub struct Side {
    pub stage: usize,
    pub client: Client,
    pub model: String,
    pub sandbox: Sandbox,
    pub permission: &'static str,
    pub workspace: Option<PathBuf>,
}
impl Side {
    pub fn of(req: &Request, workspace: Option<&Path>) -> Self {
        let role = req
            .template
            .stages()
            .get(req.stage)
            .map(|s| s.role)
            .unwrap_or(Role::Build);
        Side {
            stage: req.stage,
            client: req.profile.client,
            model: req.profile.model.trim().to_owned(),
            sandbox: req.sandbox,
            permission: permission(role),
            workspace: workspace.map(Path::to_path_buf),
        }
    }
}

/// The run that would be continued.
#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub stage: usize,
    pub stage_name: String,
    pub run: String,
    pub side: Side,
    pub status: String,
    pub session: Option<String>,
    pub resumable: bool,
    pub last_context: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    NoPrior,
    ContextOverThreshold,
    ClientDiffers,
    ModelDiffers,
    PermissionDiffers,
    WorkdirDiffers,
    SessionUnavailable,
    PriorBudgetStopped,
    SessionBusy,
    ChainBroken,
    ClientNotReusable,
    PriorNotPassed,
}
impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoPrior => "no_prior",
            Self::ContextOverThreshold => "context_over_threshold",
            Self::ClientDiffers => "client_differs",
            Self::ModelDiffers => "model_differs",
            Self::PermissionDiffers => "permission_differs",
            Self::WorkdirDiffers => "workdir_differs",
            Self::SessionUnavailable => "session_unavailable",
            Self::PriorBudgetStopped => "prior_budget_stopped",
            Self::SessionBusy => "session_busy",
            Self::ChainBroken => "chain_broken",
            Self::ClientNotReusable => "client_not_reusable",
            Self::PriorNotPassed => "prior_not_passed",
        }
    }
    /// A new session that should still carry the source's answer and hand-off digest
    /// (R2, E4, E5): the session existed but cannot or should not be continued.
    pub fn carries_previous(self) -> bool {
        matches!(self, Self::ContextOverThreshold | Self::SessionUnavailable)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Plan {
    Resume {
        session: String,
        /// Claude reusing a previous stage forks, so each stage has its own session ID.
        fork: bool,
        source_stage: usize,
        source_run: String,
        context: Option<u64>,
    },
    Fresh {
        reason: Reason,
        context: Option<u64>,
    },
}

fn same_dir(a: &Option<PathBuf>, b: &Option<PathBuf>) -> bool {
    let norm = |p: &Option<PathBuf>| {
        p.as_ref()
            .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
    };
    norm(a) == norm(b)
}

/// Decide whether `current` continues `source`'s session. The first matching rule wins.
pub fn decide(mode: Mode, current: &Side, source: Option<&Source>, threshold: u64) -> Plan {
    let fresh = |reason, context| Plan::Fresh { reason, context };
    if !matches!(current.client, Client::Codex | Client::Claude) {
        return fresh(Reason::ClientNotReusable, None);
    }
    let Some(src) = source else {
        return fresh(Reason::NoPrior, None);
    };
    if mode == Mode::Adjacent {
        let s = &src.side;
        if s.client != current.client {
            // A skipped stage in between: the run before it used another CLI (E6).
            let skipped = src.stage + 1 < current.stage;
            return fresh(
                if skipped {
                    Reason::ChainBroken
                } else {
                    Reason::ClientDiffers
                },
                None,
            );
        }
        if s.model.trim() != current.model.trim() {
            return fresh(Reason::ModelDiffers, None);
        }
        if s.sandbox != current.sandbox || s.permission != current.permission {
            return fresh(Reason::PermissionDiffers, None);
        }
        if !same_dir(&s.workspace, &current.workspace) {
            return fresh(Reason::WorkdirDiffers, None);
        }
    }
    match src.status.as_str() {
        // A stage stopped for its budget is what a follow-up continues; it is not a
        // source for later stages (E9).
        "budget_exhausted" if mode == Mode::Adjacent => {
            return fresh(Reason::PriorBudgetStopped, None);
        }
        "running" | "initializing" | "claimed" => return fresh(Reason::SessionBusy, None),
        "succeeded" => {}
        _ if mode == Mode::Adjacent => return fresh(Reason::PriorNotPassed, None),
        _ => {}
    }
    let Some(session) = src.session.clone().filter(|_| src.resumable) else {
        return fresh(Reason::SessionUnavailable, None);
    };
    if src.last_context.is_some_and(|c| c > threshold) {
        return fresh(Reason::ContextOverThreshold, src.last_context);
    }
    Plan::Resume {
        session,
        fork: current.client == Client::Claude && mode == Mode::Adjacent,
        source_stage: src.stage,
        source_run: src.run.clone(),
        context: src.last_context,
    }
}

/// Read the run `run` under `runs` as a [`Source`]: its request, evidence and the context
/// of its last model call (`last_context` in evidence, else the cached usage summary).
pub fn source(runs: &Path, stage: usize, stage_name: &str, run: &str) -> Result<Source> {
    crate::workspace::ident(run, 64)?;
    let dir = runs.join(run);
    let req: Request = serde_json::from_reader(
        std::fs::File::open(dir.join("request.json")).context("找不到前序执行的 request.json")?,
    )?;
    let ev = crate::workflow::evidence(&dir)?;
    let workspace = ev["workspace"].as_str().map(PathBuf::from);
    let last_context = ev["last_context"]
        .as_u64()
        .or_else(|| crate::usage::summary(&dir).last_context);
    Ok(Source {
        stage,
        stage_name: stage_name.to_owned(),
        run: run.to_owned(),
        side: Side::of(&req, workspace.as_deref()),
        status: ev["status"].as_str().unwrap_or_default().to_owned(),
        session: ev["session"].as_str().map(str::to_owned),
        resumable: ev["resumable"] == true,
        last_context,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn side(client: Client) -> Side {
        Side {
            stage: 2,
            client,
            model: "opus".into(),
            sandbox: Sandbox::Standard,
            permission: "write",
            workspace: Some(PathBuf::from("/tmp/aep-session-reuse-ws")),
        }
    }
    fn src(client: Client) -> Source {
        Source {
            stage: 1,
            stage_name: "交互设计".into(),
            run: "r1".into(),
            side: Side {
                stage: 1,
                ..side(client)
            },
            status: "succeeded".into(),
            session: Some("s1".into()),
            resumable: true,
            last_context: Some(90_000),
        }
    }
    fn reason(p: Plan) -> Reason {
        match p {
            Plan::Fresh { reason, .. } => reason,
            other => panic!("expected a new session, got {other:?}"),
        }
    }

    #[test]
    fn session_reuse_adjacent_claude_forks_and_codex_does_not() {
        let p = decide(
            Mode::Adjacent,
            &side(Client::Claude),
            Some(&src(Client::Claude)),
            DEFAULT_LIMIT,
        );
        assert_eq!(
            p,
            Plan::Resume {
                session: "s1".into(),
                fork: true,
                source_stage: 1,
                source_run: "r1".into(),
                context: Some(90_000)
            }
        );
        let p = decide(
            Mode::Adjacent,
            &side(Client::Codex),
            Some(&src(Client::Codex)),
            DEFAULT_LIMIT,
        );
        assert!(matches!(p, Plan::Resume { fork: false, .. }));
        let p = decide(
            Mode::Continue,
            &side(Client::Claude),
            Some(&src(Client::Claude)),
            DEFAULT_LIMIT,
        );
        assert!(matches!(p, Plan::Resume { fork: false, .. }));
    }

    #[test]
    fn session_reuse_threshold_both_branches() {
        // AC-4: at the limit the session is continued, above it a new session starts.
        for mode in [Mode::Continue, Mode::Adjacent] {
            let mut s = src(Client::Claude);
            s.last_context = Some(150_000);
            assert!(matches!(
                decide(mode, &side(Client::Claude), Some(&s), DEFAULT_LIMIT),
                Plan::Resume { .. }
            ));
            s.last_context = Some(150_001);
            assert_eq!(
                decide(mode, &side(Client::Claude), Some(&s), DEFAULT_LIMIT),
                Plan::Fresh {
                    reason: Reason::ContextOverThreshold,
                    context: Some(150_001)
                }
            );
            // Unknown context does not block.
            s.last_context = None;
            assert!(matches!(
                decide(mode, &side(Client::Claude), Some(&s), 20_000),
                Plan::Resume { context: None, .. }
            ));
        }
        assert!(Reason::ContextOverThreshold.carries_previous());
        assert!(!Reason::ModelDiffers.carries_previous());
    }

    #[test]
    fn session_reuse_reasons_for_a_new_session() {
        let cur = side(Client::Claude);
        let check = |mutate: &dyn Fn(&mut Source), expected: Reason| {
            let mut s = src(Client::Claude);
            mutate(&mut s);
            assert_eq!(
                reason(decide(Mode::Adjacent, &cur, Some(&s), DEFAULT_LIMIT)),
                expected
            );
        };
        assert_eq!(
            reason(decide(Mode::Adjacent, &cur, None, DEFAULT_LIMIT)),
            Reason::NoPrior
        );
        assert_eq!(
            reason(decide(
                Mode::Adjacent,
                &side(Client::ZCode),
                Some(&src(Client::ZCode)),
                DEFAULT_LIMIT
            )),
            Reason::ClientNotReusable
        );
        check(&|s| s.side.client = Client::Codex, Reason::ClientDiffers);
        // E6: a skipped stage between, and the run before it used another CLI.
        check(
            &|s| {
                s.stage = 0;
                s.side.client = Client::ZCode;
            },
            Reason::ChainBroken,
        );
        // E6: skipped stage but same CLI still reuses.
        let mut s = src(Client::Claude);
        s.stage = 0;
        assert!(matches!(
            decide(Mode::Adjacent, &cur, Some(&s), DEFAULT_LIMIT),
            Plan::Resume {
                source_stage: 0,
                ..
            }
        ));
        check(&|s| s.side.model = "sonnet".into(), Reason::ModelDiffers);
        // Empty means the default on both sides; whitespace is ignored.
        let mut both = side(Client::Claude);
        both.model = String::new();
        let mut s = src(Client::Claude);
        s.side.model = "  ".into();
        assert!(matches!(
            decide(Mode::Adjacent, &both, Some(&s), DEFAULT_LIMIT),
            Plan::Resume { .. }
        ));
        // E8
        check(&|s| s.side.permission = "read", Reason::PermissionDiffers);
        check(
            &|s| s.side.sandbox = Sandbox::Full,
            Reason::PermissionDiffers,
        );
        check(
            &|s| s.side.workspace = Some("/tmp/elsewhere".into()),
            Reason::WorkdirDiffers,
        );
        // E9
        check(
            &|s| s.status = "budget_exhausted".into(),
            Reason::PriorBudgetStopped,
        );
        // E10
        check(&|s| s.status = "running".into(), Reason::SessionBusy);
        // E7: a sent-back (failed) run is never a source.
        check(&|s| s.status = "failed".into(), Reason::PriorNotPassed);
        check(&|s| s.session = None, Reason::SessionUnavailable);
        check(&|s| s.resumable = false, Reason::SessionUnavailable);
    }

    #[test]
    fn session_reuse_continue_allows_a_stopped_or_failed_round() {
        let cur = side(Client::Claude);
        for status in ["budget_exhausted", "failed", "cancelled"] {
            let mut s = src(Client::Claude);
            s.status = status.into();
            // Continue ignores CLI-independent differences: it is the same stage.
            s.side.permission = "read";
            assert!(matches!(
                decide(Mode::Continue, &cur, Some(&s), DEFAULT_LIMIT),
                Plan::Resume { .. }
            ));
        }
        let mut s = src(Client::Claude);
        s.status = "running".into();
        assert_eq!(
            reason(decide(Mode::Continue, &cur, Some(&s), DEFAULT_LIMIT)),
            Reason::SessionBusy
        );
    }

    #[test]
    fn session_reuse_limit_defaults_and_floor() {
        assert_eq!(limit(None), 150_000);
        assert_eq!(limit(Some(80_000)), 80_000);
        assert_eq!(limit(Some(5_000)), MIN_LIMIT);
        assert_eq!(
            serde_json::to_value(Reason::ContextOverThreshold).unwrap(),
            "context_over_threshold"
        );
        for r in [Reason::NoPrior, Reason::ChainBroken, Reason::PriorNotPassed] {
            assert_eq!(serde_json::to_value(r).unwrap(), r.as_str());
        }
        assert_eq!(permission(Role::Plan), "write");
        assert_eq!(permission(Role::Test), "gate");
        assert_eq!(permission(Role::Deliver), "read");
    }
    #[test]
    fn session_reuse_source_reads_request_evidence_and_usage() {
        let t = tempfile::tempdir().unwrap();
        let runs = t.path().join("runs");
        let request = |dir: &Path| {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(
                dir.join("request.json"),
                serde_json::json!({
                    "sandbox":"Network","project":"p","work":"w","template":"Software","stage":1,
                    "profile":{"client":"Claude","executable":"","model":" opus "},
                    "repository":null,"brief":"","approved":[]
                })
                .to_string(),
            )
            .unwrap();
        };
        let a = runs.join("a1");
        request(&a);
        std::fs::write(
            a.join("evidence.json"),
            r#"{"status":"succeeded","session":"s-a","resumable":true,"workspace":"/tmp/ws","last_context":42000}"#,
        )
        .unwrap();
        let s = source(&runs, 1, "交互设计", "a1").unwrap();
        assert_eq!(s.side.client, Client::Claude);
        assert_eq!(s.side.model, "opus");
        assert_eq!(s.side.sandbox, Sandbox::Network);
        assert_eq!(s.side.permission, "write");
        assert_eq!(s.side.workspace, Some(PathBuf::from("/tmp/ws")));
        assert_eq!(s.session.as_deref(), Some("s-a"));
        assert!(s.resumable);
        assert_eq!(s.last_context, Some(42_000));
        assert_eq!(s.stage_name, "交互设计");
        // Older evidence has no `last_context`: the cached usage summary supplies it.
        let b = runs.join("b2");
        request(&b);
        std::fs::write(
            b.join("evidence.json"),
            r#"{"status":"succeeded","session":"s-b","resumable":true}"#,
        )
        .unwrap();
        let cached = crate::usage::Summary {
            rev: crate::usage::REV,
            last_context: Some(200_000),
            ..Default::default()
        };
        std::fs::write(b.join("usage.json"), serde_json::to_vec(&cached).unwrap()).unwrap();
        let s = source(&runs, 1, "交互设计", "b2").unwrap();
        assert_eq!(s.last_context, Some(200_000));
        let cur = Side {
            stage: 2,
            workspace: None,
            ..s.side.clone()
        };
        assert_eq!(
            decide(Mode::Adjacent, &cur, Some(&s), DEFAULT_LIMIT),
            Plan::Fresh {
                reason: Reason::ContextOverThreshold,
                context: Some(200_000)
            }
        );
        assert!(source(&runs, 1, "x", "../escape").is_err());
    }
}
