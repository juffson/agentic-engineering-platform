use crate::{
    Runner,
    relay::{self, Round},
};
use aep::{
    studio::{Artifact, artifact_file},
    workspace::{Workspace, commit, status},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::{File, OpenOptions},
    io::Read,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

pub(crate) use aep::delivery::{Pull, Target};

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Publication {
    pub target: Target,
    pub confirmed_by: String,
    pub confirmed_at_unix_seconds: u64,
    pub pushed: bool,
    pub pr: Option<Pull>,
    pub error: Option<String>,
}

pub(crate) struct Review {
    pub target: Target,
    pub diff: Vec<String>,
    pub checks: Vec<String>,
    pub report: Vec<String>,
    pub publication: Option<Publication>,
}

pub(crate) fn command(
    ws: &Workspace,
    runner: &Runner,
    program: &str,
    args: &[&str],
) -> Result<Vec<u8>> {
    aep::delivery::command(ws, &runner.path()?, program, args)
}

fn git(ws: &Workspace, runner: &Runner, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8_lossy(&command(ws, runner, "git", args)?).into_owned())
}

pub(crate) fn github(ws: &Workspace, runner: &Runner, args: &[&str]) -> Result<Value> {
    aep::delivery::github(ws, &runner.path()?, args)
}

pub(crate) fn repository(ws: &Workspace, runner: &Runner) -> Result<String> {
    aep::delivery::repository(ws, &runner.path()?)
}

fn candidate(ws: &Workspace, relay_id: &str, id: &str) -> Result<Value> {
    let relay = relay::load_one(ws, relay_id)?;
    ensure!(
        relay
            .latest(Round::Second)
            .is_some_and(|attempt| attempt.id == id),
        "此 Run 不是当前接力的第二轮"
    );
    let record = status(&ws.data, id)?;
    ensure!(
        record["status"] == "awaiting_human_review"
            && record["check_exit_code"] == 0
            && relay.agents.matches_evidence(&record)
            && record["review_status"] == "findings_recorded"
            && record["task"] == relay.task.name
            && record["spec_sha"] == relay.task.sha,
        "第二轮需先完成验收和审查"
    );
    let reference = format!("refs/aep/runs/{id}");
    ensure!(
        record["candidate_sha"] == commit(&ws.root, &reference)?,
        "候选引用与第二轮证据不一致"
    );
    Ok(record)
}

pub(crate) fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .with_context(|| format!("缺少 {key}"))
}

pub(crate) fn load(path: &Path) -> Result<Option<Publication>> {
    match File::open(path.join("publication.json")) {
        Ok(file) => Ok(Some(serde_json::from_reader(file)?)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn save(path: &Path, publication: &Publication) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(path)?;
    serde_json::to_writer_pretty(&mut file, publication)?;
    file.persist(path.join("publication.json"))?;
    Ok(())
}

fn pages(text: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut remaining = text;
    while remaining.len() > 12 * 1024 {
        let mut end = 12 * 1024;
        while !remaining.is_char_boundary(end) {
            end -= 1;
        }
        let end = remaining[..end]
            .rfind('\n')
            .map_or(end, |newline| newline + 1);
        chunks.push(remaining[..end].to_owned());
        remaining = &remaining[end..];
    }
    chunks.push(remaining.to_owned());
    chunks
}

fn artifact(ws: &Workspace, id: &str, kind: Artifact) -> Result<Vec<String>> {
    let mut file =
        artifact_file(&ws.data, id, kind)?.with_context(|| format!("缺少 {}", kind.filename()))?;
    let mut contents = Vec::new();
    file.read_to_end(&mut contents)?;
    Ok(pages(&String::from_utf8_lossy(&contents)))
}

pub(crate) fn prepare(
    ws: &Workspace,
    runner: &Runner,
    relay: &str,
    id: &str,
    requested_base: &str,
) -> Result<Review> {
    let run = candidate(ws, relay, id)?;
    let path = ws.data.join("runs").join(id);
    let publication = load(&path)?;
    let sha = field(&run, "candidate_sha")?;
    let task = field(&run, "task")?;
    let spec = field(&run, "spec_sha")?;
    let target = if let Some(publication) = publication.as_ref().filter(|saved| saved.pr.is_some())
    {
        ensure!(
            publication.target.candidate == sha
                && publication.target.run == id
                && publication.target.relay == relay,
            "历史确认与当前运行不一致"
        );
        publication.target.clone()
    } else {
        let repo = repository(ws, runner)?;
        let base = if requested_base.is_empty() {
            let info = github(ws, runner, &[&format!("repos/{repo}")])?;
            field(&info, "default_branch")?.to_owned()
        } else {
            requested_base.to_owned()
        };
        let base_ref = format!("refs/heads/{base}");
        git(ws, runner, &["check-ref-format", &base_ref])?;
        let local_ref = format!("refs/aep/publish/{id}/preview");
        git(
            ws,
            runner,
            &[
                "fetch",
                "--no-tags",
                "--no-write-fetch-head",
                "origin",
                &format!("+{base_ref}:{local_ref}"),
            ],
        )?;
        let base_sha = commit(&ws.root, &local_ref)?;
        let merge_base = git(ws, runner, &["merge-base", &base_sha, sha])?
            .trim()
            .to_owned();
        Target {
            relay: relay.into(),
            run: id.into(),
            task: task.into(),
            repo,
            base,
            base_sha,
            merge_base,
            candidate: sha.into(),
            spec: spec.into(),
            head: format!("aep/pr/{task}/{id}"),
        }
    };
    let diff = git(
        ws,
        runner,
        &[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            &target.merge_base,
            &target.candidate,
            "--",
        ],
    )?;
    ensure!(
        !diff.is_empty(),
        "最终候选与 GitHub 目标分支没有可提交的 Diff"
    );
    Ok(Review {
        target,
        diff: pages(&diff),
        checks: artifact(ws, id, Artifact::Checks)?,
        report: artifact(ws, id, Artifact::Review)?,
        publication,
    })
}

pub(crate) fn remote_sha(
    ws: &Workspace,
    runner: &Runner,
    reference: &str,
) -> Result<Option<String>> {
    aep::delivery::remote_sha(ws, &runner.path()?, reference)
}

fn check_base(ws: &Workspace, runner: &Runner, target: &Target) -> Result<()> {
    aep::delivery::check_base(ws, &runner.path()?, target)
}

fn submit(ws: &Workspace, runner: &Runner, path: &Path, item: &mut Publication) -> Result<()> {
    let target = item.target.clone();
    let pull = aep::delivery::submit(ws, &runner.path()?, &target, || {
        item.pushed = true;
        save(path, item)
    })?;
    let matches = pull.head_sha == target.candidate
        && pull.base == target.base
        && pull.base_sha == target.base_sha
        && pull.state == "open";
    item.pr = Some(pull);
    ensure!(
        matches,
        "GitHub PR 的分支版本或状态已变化，请打开该 PR 核对"
    );
    Ok(())
}

pub(crate) fn publish(ws: &Workspace, runner: &Runner, target: Target) -> Result<Publication> {
    let run = candidate(ws, &target.relay, &target.run)?;
    ensure!(
        run["candidate_sha"] == target.candidate
            && run["spec_sha"] == target.spec
            && run["task"] == target.task,
        "当前候选与人工确认的版本不一致"
    );
    let path = ws.data.join("runs").join(&target.run);
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.join("publish.lock"))?;
    lock.try_lock().context("此候选正在发布")?;
    let previous = load(&path)?;
    if let Some(previous) = previous.as_ref().filter(|saved| saved.pr.is_some()) {
        ensure!(previous.target == target, "此候选已有 PR");
        return Ok(previous.clone());
    }
    ensure!(
        repository(ws, runner)? == target.repo,
        "origin 已更换，请重新审查"
    );
    check_base(ws, runner, &target)?;
    let user = github(ws, runner, &["user"])?;
    let mut publication = Publication {
        confirmed_by: field(&user, "login")?.into(),
        confirmed_at_unix_seconds: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        pushed: previous.is_some_and(|saved| saved.target.repo == target.repo && saved.pushed),
        target,
        pr: None,
        error: None,
    };
    let base_ref = format!("refs/aep/publish/{}/confirmed", publication.target.run);
    git(
        ws,
        runner,
        &["update-ref", &base_ref, &publication.target.base_sha],
    )?;
    save(&path, &publication)?;
    if let Err(error) = submit(ws, runner, &path, &mut publication) {
        publication.error = Some(format!("{error:#}"));
    }
    save(&path, &publication)?;
    Ok(publication)
}
