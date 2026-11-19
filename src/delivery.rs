//! GitHub delivery for a candidate that a trusted caller has already authorized.
use crate::workspace::Workspace;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{ffi::OsStr, process::Command};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Target {
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
pub struct Pull {
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

pub fn command(ws: &Workspace, path: &OsStr, program: &str, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new(program)
        .current_dir(&ws.root)
        .args(args)
        .env("PATH", path)
        .env_remove(crate::CONTROL_OPERATOR_TOKEN)
        .env_remove(crate::CONTROL_DEVICE_TOKEN)
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

fn git(ws: &Workspace, path: &OsStr, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8_lossy(&command(ws, path, "git", args)?).into_owned())
}

pub fn github(ws: &Workspace, path: &OsStr, args: &[&str]) -> Result<Value> {
    let mut command_args = vec!["api", "--hostname", "github.com"];
    command_args.extend(args);
    Ok(serde_json::from_slice(&command(
        ws,
        path,
        "gh",
        &command_args,
    )?)?)
}

pub fn slug(remote: &str) -> Result<String> {
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

pub fn repository(ws: &Workspace, path: &OsStr) -> Result<String> {
    let fetch = git(ws, path, &["remote", "get-url", "origin"])?;
    let pushed = git(
        ws,
        path,
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

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .with_context(|| format!("missing {key}"))
}

pub fn remote_sha(ws: &Workspace, path: &OsStr, reference: &str) -> Result<Option<String>> {
    let output = git(ws, path, &["ls-remote", "--refs", "origin", reference])?;
    Ok(output
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .find(|(_, name)| *name == reference)
        .map(|(sha, _)| sha.to_owned()))
}

pub fn check_base(ws: &Workspace, path: &OsStr, target: &Target) -> Result<()> {
    ensure!(
        remote_sha(ws, path, &format!("refs/heads/{}", target.base))?.as_deref()
            == Some(&target.base_sha),
        "GitHub 目标分支已更新，请重新加载 Diff 并确认"
    );
    Ok(())
}

pub fn check_spec(ws: &Workspace, path: &OsStr, target: &Target) -> Result<()> {
    ensure!(
        remote_sha(ws, path, &format!("refs/heads/aep-task/{}", target.task))?.as_deref()
            == Some(&target.spec),
        "远端派发的 Spec 版本已变化，请重新验收并确认"
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

fn existing(ws: &Workspace, path: &OsStr, target: &Target) -> Result<Option<Pull>> {
    let owner = target
        .repo
        .split_once('/')
        .context("GitHub 仓库地址无效")?
        .0;
    let endpoint = format!("repos/{}/pulls", target.repo);
    let prs = github(
        ws,
        path,
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
    let prs = prs.as_array().context("GitHub PR 列表无效")?;
    ensure!(prs.len() <= 1, "候选分支关联多个 GitHub PR，请人工核对");
    prs.first().map(parse_pull).transpose()
}

fn create(ws: &Workspace, path: &OsStr, target: &Target) -> Result<Pull> {
    let body = format!(
        "AEP local review confirmed for the exact candidate.\n\n- Task: `{}`\n- Run: `{}`\n- Spec: `{}`\n- Candidate: `{}`\n- Local acceptance: passed\n",
        target.task, target.run, target.spec, target.candidate
    );
    match github(
        ws,
        path,
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
        Err(error) => existing(ws, path, target)?.ok_or(error),
    }
}

pub fn submit(
    ws: &Workspace,
    path: &OsStr,
    target: &Target,
    pushed: impl FnOnce() -> Result<()>,
) -> Result<Pull> {
    check_base(ws, path, target)?;
    check_spec(ws, path, target)?;
    let reference = format!("refs/heads/{}", target.head);
    let current = remote_sha(ws, path, &reference)?;
    if let Some(sha) = current {
        ensure!(sha == target.candidate, "远端候选分支已指向另一提交");
    } else {
        ensure!(
            existing(ws, path, target)?.is_none(),
            "该候选分支已有 PR，请在 GitHub 查看"
        );
        git(
            ws,
            path,
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
    pushed()?;
    check_base(ws, path, target)?;
    check_spec(ws, path, target)?;
    ensure!(
        remote_sha(ws, path, &reference)?.as_deref() == Some(&target.candidate),
        "远端候选分支已更新，请核对再发布"
    );
    match existing(ws, path, target)? {
        Some(pr) => Ok(pr),
        None => create(ws, path, target),
    }
}
