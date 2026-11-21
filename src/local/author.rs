use super::{Pin, Repo, Ticket, existing, git, lock, propose, target_commit};
use aep::{CONTROL_DEVICE_TOKEN, CONTROL_OPERATOR_TOKEN, task::TaskDraft};
use anyhow::{Context, Result, ensure};
use clap::Args;
use serde::Serialize;
use std::{
    fs::File,
    io::{self, Read, Seek, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Args)]
pub(crate) struct Request {
    #[arg(long)]
    ticket: String,
    #[arg(long, default_value = "main")]
    target: String,
    /// Commit currently displayed for the chosen local branch.
    #[arg(long)]
    baseline: String,
    /// JSON task draft. Use '-' for standard input.
    #[arg(long, default_value = "-")]
    draft: PathBuf,
    /// Current plan version when revising an existing ticket.
    #[arg(long)]
    version: Option<u64>,
    #[arg(long)]
    codex_model: Option<String>,
    /// Clear a previously pinned model and use the installed client's default.
    #[arg(long, conflicts_with = "codex_model")]
    default_codex_model: bool,
}

#[derive(Serialize)]
pub(super) struct Target {
    name: String,
    sha: String,
    checked_out: bool,
}

pub(super) fn targets(repo: &Repo) -> Result<Vec<Target>> {
    let refs = git(
        &repo.root,
        &[
            "for-each-ref",
            "--format=%(refname:lstrip=2)%00%(objectname)%00%(HEAD)",
            "refs/heads/",
        ],
    )?;
    refs.lines()
        .map(|line| {
            let mut fields = line.split('\0');
            Ok(Target {
                name: fields.next().context("missing branch")?.into(),
                sha: fields.next().context("missing commit")?.into(),
                checked_out: fields.next() == Some("*"),
            })
        })
        .collect()
}

fn read_draft(input: &Path) -> Result<TaskDraft> {
    const LIMIT: u64 = 1024 * 1024;
    let source: Box<dyn Read> = if input == Path::new("-") {
        Box::new(io::stdin().lock())
    } else {
        Box::new(File::open(input).context("open task draft")?)
    };
    let mut data = Vec::new();
    source.take(LIMIT + 1).read_to_end(&mut data)?;
    ensure!(data.len() as u64 <= LIMIT, "task draft exceeds 1 MiB");
    let draft: TaskDraft = serde_json::from_slice(&data).context("parse task draft")?;
    draft.validate()?;
    ensure!(
        draft
            .manifest
            .title
            .as_deref()
            .is_some_and(|title| !title.trim().is_empty()),
        "the implementation task needs a title"
    );
    Ok(draft)
}

struct Index<'a> {
    root: &'a Path,
    file: PathBuf,
}

impl Index<'_> {
    fn git(&self, args: &[&str], data: Option<&[u8]>) -> Result<Vec<u8>> {
        let stdin = if let Some(data) = data {
            let mut file = tempfile::tempfile()?;
            file.write_all(data)?;
            file.rewind()?;
            Stdio::from(file)
        } else {
            Stdio::null()
        };
        let result = Command::new("git")
            .arg("-C")
            .arg(self.root)
            .args(args)
            .env("GIT_INDEX_FILE", &self.file)
            .env_remove(CONTROL_OPERATOR_TOKEN)
            .env_remove(CONTROL_DEVICE_TOKEN)
            .stdin(stdin)
            .output()
            .context("start Git for local Spec")?;
        ensure!(
            result.status.success(),
            "git {}: {}",
            args[0],
            String::from_utf8_lossy(&result.stderr).trim()
        );
        Ok(result.stdout)
    }

    fn text(&self, args: &[&str], data: Option<&[u8]>) -> Result<String> {
        Ok(String::from_utf8(self.git(args, data)?)?.trim_end().into())
    }
}

fn spec_commit(
    repo: &Repo,
    baseline: &str,
    previous: Option<&Pin>,
    draft: &TaskDraft,
) -> Result<String> {
    let tmp = tempfile::tempdir().context("allocate isolated Git index")?;
    let index = Index {
        root: &repo.root,
        file: tmp.path().join("index"),
    };
    let prefix = format!("specs/{}", draft.name);
    let parent = previous
        .filter(|old| old.baseline == baseline)
        .map_or(baseline, |old| old.spec.as_str());
    index.git(&["read-tree", parent], None)?;
    if let Some(old) = previous.filter(|old| old.baseline != baseline) {
        let changes = index.git(
            &[
                "diff",
                "--binary",
                "--no-ext-diff",
                "--no-textconv",
                &old.baseline,
                &old.spec,
                "--",
                &prefix,
            ],
            None,
        )?;
        if !changes.is_empty() {
            index.git(&["apply", "--cached", "--3way", "--whitespace=nowarn", "-"], Some(&changes))
                .context("the saved Spec conflicts with the updated target; resolve it in a local Spec branch")?;
        }
    }
    let manifest = serde_json::to_vec_pretty(&draft.manifest)?;
    for (name, data) in [
        ("spec.md", draft.spec.as_bytes()),
        ("task.json", manifest.as_slice()),
    ] {
        let blob = index.text(&["hash-object", "-w", "--stdin"], Some(data))?;
        index.git(
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{blob},{prefix}/{name}"),
            ],
            None,
        )?;
    }
    let tree = index.text(&["write-tree"], None)?;
    index.text(
        &[
            "-c",
            "commit.gpgsign=false",
            "commit-tree",
            &tree,
            "-p",
            parent,
            "-m",
            &format!("Specify {}", draft.name),
        ],
        None,
    )
}

pub(super) fn create(repo: &Repo, request: Request) -> Result<Ticket> {
    let draft = read_draft(&request.draft)?;
    let _guard = lock(repo, &request.ticket)?;
    let baseline = target_commit(repo, &request.target)?;
    ensure!(
        baseline == request.baseline,
        "target branch moved; reload its current commit before saving the Spec"
    );
    let prior = existing(repo, &request.ticket)?;
    let old = prior.as_ref().map(|ticket| &ticket.current);
    if let Some(saved) = old {
        ensure!(
            saved.pin.task == draft.name,
            "a Spec revision must keep the task ID"
        );
    } else {
        ensure!(request.version.is_none(), "the task ticket does not exist");
    }
    let previous = old.map(|plan| &plan.pin);
    let model = request.codex_model.or_else(|| {
        previous
            .filter(|_| !request.default_codex_model)
            .and_then(|pin| pin.codex_model.clone())
    });
    if let Some(current) = old {
        let pin = &current.pin;
        let (saved_spec, saved_manifest) = repo.task(&pin.spec, &pin.task)?;
        let same = pin.baseline == baseline
            && pin.target == request.target
            && pin.codex_model == model
            && saved_spec == draft.spec
            && saved_manifest == draft.manifest;
        let submitted_version = request.version.unwrap_or(0);
        if same
            && (request.version == Some(current.version)
                || submitted_version.checked_add(1) == Some(current.version))
        {
            return propose(repo, request.ticket, pin.clone(), prior, false);
        }
        ensure!(
            request.version == Some(current.version),
            "task plan version changed; reopen it before revising"
        );
        ensure!(
            current.merging.is_none() || current.merged_at.is_some(),
            "reconcile the pending merge before revising"
        );
    }
    let spec = spec_commit(repo, &baseline, previous, &draft)?;
    let pin = Pin::read(
        repo,
        draft.name,
        &spec,
        request.target,
        previous.map_or(600, |pin| pin.timeout),
        previous.map_or(3, |pin| pin.max_attempts),
        (
            previous.and_then(|pin| pin.max_tokens),
            previous.and_then(|pin| pin.max_cost_micros),
            model,
        ),
    )?;
    ensure!(
        pin.baseline == baseline,
        "target branch moved while preparing the Spec"
    );
    propose(repo, request.ticket, pin, prior, request.version.is_some())
}
