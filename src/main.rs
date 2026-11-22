mod agent;
mod control;
mod git;
use aep::memory::{self, Key, Kind};
mod local;
mod project_cli;
mod run;
mod workflow_stage;
mod zcode_desktop;
mod zcode_stage;

use anyhow::{Result, ensure};
use clap::{Args, Parser, Subcommand};
use serde_json::json;
use std::{path::PathBuf, process::ExitCode};

#[derive(Parser)]
#[command(version, about = "Run pinned Git tasks and share engineering memory")]
struct Cli {
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Manage Studio projects, run a stage or exchange handoffs with independent sessions.
    Project(project_cli::Options),
    #[command(hide = true)]
    ZcodeStage {
        #[arg(long)]
        binary: PathBuf,
        #[arg(long)]
        request: PathBuf,
    },
    #[command(hide = true)]
    ZcodeBridge {
        #[arg(long)]
        binary: PathBuf,
        #[arg(long)]
        mode: String,
        #[arg(long)]
        model: Option<PathBuf>,
        #[arg(long)]
        prompt: PathBuf,
        #[arg(long)]
        dir: PathBuf,
        #[arg(long)]
        desktop_workspace: Option<PathBuf>,
    },
    /// Run a single configured model CLI in a project workflow.
    WorkflowStage {
        #[arg(long)]
        request: PathBuf,
    },
    Control(control::Control),
    /// Review and deliver a pinned task entirely in the local repository.
    Local {
        #[command(subcommand)]
        action: local::Action,
    },
    Dispatch {
        #[arg(long)]
        task: String,
        #[arg(long, default_value = "HEAD")]
        rev: String,
        #[arg(long, default_value = "origin")]
        remote: String,
    },
    Run {
        #[command(flatten)]
        args: Execution,
        #[arg(long, default_value = "origin")]
        remote: String,
        /// Use this pinned Spec commit for an already dispatched task.
        #[arg(long, conflicts_with = "remote")]
        base: Option<String>,
        #[arg(long, value_parser=["codex","zcode"], conflicts_with="candidate", required_unless_present="candidate")]
        implementer: Option<String>,
        /// Start the implementer from an existing candidate descended from the Spec.
        #[arg(long, requires = "implementer")]
        resume_from: Option<String>,
        #[arg(long, requires = "implementer")]
        codex_model: Option<String>,
        /// Pin the ZCode client with provider/model or provider/model@reasoning-level.
        #[arg(long)]
        zcode_model: Option<String>,
    },
    Verify {
        #[command(flatten)]
        args: Execution,
        #[arg(long)]
        base: String,
    },
    Status {
        #[arg(long)]
        run_id: String,
    },
    Memory {
        #[command(subcommand)]
        operation: MemoryAction,
    },
}
#[derive(Args)]
struct Execution {
    #[arg(long)]
    task: String,
    #[arg(long)]
    candidate: Option<String>,
    #[arg(long)]
    run_id: Option<String>,
    #[arg(long)]
    shared_memory: bool,
    /// Require this current memory version in each model client's context.
    #[arg(long, requires = "shared_memory")]
    require_memory: Option<i64>,
    #[arg(long, default_value="manual", value_parser=["manual","claude","zcode"])]
    reviewer: String,
    #[arg(long, default_value_t=600, value_parser=clap::value_parser!(u64).range(1..))]
    timeout: u64,
    #[arg(skip)]
    whole_attempt: bool,
    #[arg(skip)]
    feedback_run: Option<String>,
    #[arg(skip)]
    budget: Option<local::Budget>,
}
#[derive(Args)]
struct Scope {
    #[arg(long, conflicts_with_all=["task","spec","candidate"], required_unless_present="task")]
    project: bool,
    #[arg(long, requires = "spec")]
    task: Option<String>,
    #[arg(long)]
    spec: Option<String>,
    #[arg(long)]
    candidate: Option<String>,
}
#[derive(Subcommand)]
enum MemoryAction {
    Get {
        #[arg(long)]
        id: i64,
    },
    List {
        #[command(flatten)]
        scope: Scope,
    },
    Add {
        #[command(flatten)]
        scope: Scope,
        #[arg(long)]
        kind: Kind,
        #[arg(long)]
        text: String,
        #[arg(long)]
        replaces: Option<i64>,
    },
}
fn app() -> Result<ExitCode> {
    let cli = Cli::parse();
    let action = match cli.command {
        Action::Project(opt) => {
            project_cli::execute(opt, &cli.repo)?;
            return Ok(ExitCode::SUCCESS);
        }
        Action::ZcodeStage { binary, request } => {
            zcode_stage::run(&binary, &request)?;
            return Ok(ExitCode::SUCCESS);
        }
        Action::ZcodeBridge {
            binary,
            mode,
            model,
            prompt,
            dir,
            desktop_workspace,
        } => {
            let selection = model.map(|path| aep::workspace::read(&path)).transpose()?;
            let text = std::fs::read_to_string(&prompt)?;
            let workspace = desktop_workspace.unwrap_or(std::env::current_dir()?);
            let reply =
                zcode_stage::converse(&binary, &dir, &mode, selection.as_ref(), &text, &workspace)?;
            println!("{reply}");
            return Ok(ExitCode::SUCCESS);
        }
        Action::WorkflowStage { request } => {
            workflow_stage::execute(&request)?;
            return Ok(ExitCode::SUCCESS);
        }
        Action::Control(request) => {
            control::execute(&cli.repo, request)?;
            return Ok(ExitCode::SUCCESS);
        }
        action => action,
    };
    let repo = git::Repo::open(&cli.repo)?;
    match action {
        Action::Project(_)
        | Action::Control(_)
        | Action::ZcodeStage { .. }
        | Action::ZcodeBridge { .. }
        | Action::WorkflowStage { .. } => {
            unreachable!()
        }
        Action::Local { action } => return local::execute(&repo, action),
        Action::Dispatch { task, rev, remote } => repo.dispatch(&task, &rev, &remote)?,
        Action::Run {
            mut args,
            remote,
            base,
            implementer,
            resume_from,
            codex_model,
            zcode_model,
        } => {
            if resume_from.is_some() {
                args.candidate = resume_from;
            }
            return run::execute(
                &repo,
                &args,
                base.as_deref()
                    .map_or(run::Base::Remote(&remote), run::Base::Local),
                implementer.as_deref(),
                codex_model.as_deref(),
                zcode_model.as_deref(),
            );
        }
        Action::Verify { args, base } => {
            ensure!(args.candidate.is_some(), "verify requires --candidate");
            return run::execute(&repo, &args, run::Base::Local(&base), None, None, None);
        }
        Action::Status { run_id } => println!(
            "{}",
            serde_json::to_string_pretty(&run::status(&repo, &run_id)?)?
        ),
        Action::Memory { operation } => {
            let mut db = memory::Memory::open(&repo.data)?;
            let value = match operation {
                MemoryAction::Get { id } => db.get(id)?,
                MemoryAction::List { scope } => {
                    let key = Key::resolve(
                        &repo.root,
                        scope.task.as_deref(),
                        scope.spec.as_deref(),
                        scope.candidate.as_deref(),
                    )?;
                    json!(db.context(&key, None)?)
                }
                MemoryAction::Add {
                    scope,
                    kind,
                    text,
                    replaces,
                } => {
                    let key = Key::resolve(
                        &repo.root,
                        scope.task.as_deref(),
                        scope.spec.as_deref(),
                        scope.candidate.as_deref(),
                    )?;
                    json!({"id": db.add(&key,kind.as_str(),&text,replaces)?})
                }
            };
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
    }
    Ok(ExitCode::SUCCESS)
}
fn main() -> ExitCode {
    match app() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}
