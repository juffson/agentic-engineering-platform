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
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Target {
    pub relay: String,
    pub run: String,
    pub task: String,
    pub repo: String,
    pub base: String,
    pub base_sha: String,
    pub merge_base: String,
    pub candidate: String,
    pub spec: String,
    pub head: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Pull {
    pub number: u64,
    pub state: String,
    pub head_sha: String,
    pub base: String,
    pub base_sha: String,
}
impl Pull {
    pub fn url(&self, target: &Target) -> String {
        format!("https://github.com/{}/pull/{}", target.repo, self.number)
    }
}

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
    let output = Command::new(program)
        .current_dir(&ws.root)
        .args(args)
        .env("PATH", runner.path()?)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GH_PROMPT_DISABLED", "1")
        .output()
        .with_context(|| format!("启动 {program}"))?;
    ensure!(
        output.status.success(),
        "{program} {}: {}",
        args[0],
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output.stdout)
}

fn git(ws: &Workspace, runner: &Runner, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8_lossy(&command(ws, runner, "git", args)?).into_owned())
}

pub(crate) fn github(ws: &Workspace, runner: &Runner, args: &[&str]) -> Result<Value> {
    let mut command_args = vec!["api", "--hostname", "github.com"];
    command_args.extend(args);
    Ok(serde_json::from_slice(&command(
        ws,
        runner,
        "gh",
        &command_args,
    )?)?)
}

fn slug(remote: &str) -> Result<String> {
    let path = [
        "https://github.com/",
        "git@github.com:",
        "ssh://git@github.com/",
    ]
    .iter()
    .find_map(|prefix| remote.strip_prefix(prefix))
    .context("origin 需要指向 github.com 上的仓库")?;
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, repo) = path.split_once('/').context("GitHub 仓库地址无效")?;
    ensure!(
        [owner, repo].iter().all(|part| {
            !part.is_empty()
                && !matches!(*part, "." | "..")
                && part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        }),
        "GitHub 仓库地址无效"
    );
    Ok(path.to_ascii_lowercase())
}

pub(crate) fn repository(ws: &Workspace, runner: &Runner) -> Result<String> {
    let fetch = git(ws, runner, &["remote", "get-url", "origin"])?;
    let pushed = git(
        ws,
        runner,
        &["remote", "get-url", "--push", "--all", "origin"],
    )?;
    ensure!(pushed.lines().count() == 1, "origin 必须只有一个推送地址");
    let repo = slug(fetch.trim())?;
    ensure!(
        slug(pushed.trim())? == repo,
        "origin 的取回和推送地址指向不同的仓库"
    );
    Ok(repo)
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
            && record["reviewer"] == "claude"
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
    let output = git(ws, runner, &["ls-remote", "--refs", "origin", reference])?;
    Ok(output
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .find(|(_, name)| *name == reference)
        .map(|(sha, _)| sha.to_owned()))
}

fn check_base(ws: &Workspace, runner: &Runner, target: &Target) -> Result<()> {
    ensure!(
        remote_sha(ws, runner, &format!("refs/heads/{}", target.base))?.as_deref()
            == Some(&target.base_sha),
        "GitHub 目标分支已更新，请重新加载 Diff 并确认"
    );
    Ok(())
}

fn parse_pull(value: &Value) -> Result<Pull> {
    Ok(Pull {
        number: value["number"].as_u64().context("GitHub PR 编号缺失")?,
        state: field(value, "state")?.into(),
        head_sha: field(&value["head"], "sha")?.into(),
        base: field(&value["base"], "ref")?.into(),
        base_sha: field(&value["base"], "sha")?.into(),
    })
}

fn existing(ws: &Workspace, runner: &Runner, target: &Target) -> Result<Option<Pull>> {
    let owner = target
        .repo
        .split_once('/')
        .context("GitHub 仓库地址无效")?
        .0;
    let endpoint = format!("repos/{}/pulls", target.repo);
    let prs = github(
        ws,
        runner,
        &[
            &endpoint,
            "--method",
            "GET",
            "-f",
            "state=all",
            "-f",
            &format!("head={owner}:{}", target.head),
        ],
    )?;
    prs.as_array()
        .context("GitHub PR 列表无效")?
        .first()
        .map(parse_pull)
        .transpose()
}

fn create(ws: &Workspace, runner: &Runner, target: &Target) -> Result<Pull> {
    let body = format!(
        "AEP Studio local review confirmed for the exact candidate.\n\n- Task: `{}`\n- Run: `{}`\n- Spec: `{}`\n- Candidate: `{}`\n- Local acceptance: passed\n",
        target.task, target.run, target.spec, target.candidate
    );
    match github(
        ws,
        runner,
        &[
            &format!("repos/{}/pulls", target.repo),
            "--method",
            "POST",
            "-f",
            &format!("title=AEP: {}", target.task),
            "-f",
            &format!("base={}", target.base),
            "-f",
            &format!("head={}", target.head),
            "-f",
            &format!("body={body}"),
        ],
    ) {
        Ok(value) => parse_pull(&value),
        Err(error) => existing(ws, runner, target)?.ok_or(error),
    }
}

fn submit(ws: &Workspace, runner: &Runner, path: &Path, item: &mut Publication) -> Result<()> {
    let target = &item.target;
    check_base(ws, runner, target)?;
    let reference = format!("refs/heads/{}", target.head);
    let remote = remote_sha(ws, runner, &reference)?;
    if let Some(remote) = remote {
        ensure!(remote == target.candidate, "远端候选分支已指向另一提交");
    } else {
        ensure!(
            existing(ws, runner, target)?.is_none(),
            "该候选分支已有 PR，请在 GitHub 查看"
        );
        git(
            ws,
            runner,
            &[
                "push",
                "--porcelain",
                "--no-follow-tags",
                "--recurse-submodules=no",
                &format!("--force-with-lease={reference}:"),
                "origin",
                &format!("{}:{reference}", target.candidate),
            ],
        )?;
    }
    item.pushed = true;
    save(path, item)?;
    check_base(ws, runner, target)?;
    ensure!(
        remote_sha(ws, runner, &reference)?.as_deref() == Some(&target.candidate),
        "远端候选分支已更新，请核对再发布"
    );
    let pull = match existing(ws, runner, target)? {
        Some(pr) => pr,
        None => create(ws, runner, target)?,
    };
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
