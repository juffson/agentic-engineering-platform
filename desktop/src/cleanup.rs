use crate::{
    Runner, github_pr,
    publish::{self, Publication, Target},
    relay::{self, Attempt, Round},
};
use aep::workspace::{Workspace, commit, ident, status};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Step {
    pub done: bool,
    pub detail: String,
}
impl Step {
    fn from(result: Result<String>) -> Self {
        match result {
            Ok(detail) => Self { done: true, detail },
            Err(error) => Self {
                done: false,
                detail: format!("{error:#}"),
            },
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Temporary {
    pub run: String,
    pub step: Step,
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Receipt {
    pub pr: u64,
    pub candidate: String,
    pub base: String,
    pub merged_sha: String,
    pub merged_by: String,
    pub merged_at: String,
    pub finished_at_unix_seconds: u64,
    pub target: Step,
    pub branch: Option<Step>,
    pub worktrees: Vec<Temporary>,
}
impl Receipt {
    pub fn complete(&self) -> bool {
        self.target.done
            && self.branch.as_ref().is_some_and(|step| step.done)
            && self.worktrees.iter().all(|entry| entry.step.done)
    }
}

pub(crate) fn read(path: &Path) -> Result<Option<Receipt>> {
    match File::open(path.join("cleanup.json")) {
        Ok(file) => Ok(Some(serde_json::from_reader(file)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn save(path: &Path, receipt: &Receipt) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(path)?;
    serde_json::to_writer_pretty(&mut file, receipt)?;
    file.persist(path.join("cleanup.json"))?;
    Ok(())
}

fn git(ws: &Workspace, runner: &Runner, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8(publish::command(
        ws, runner, "git", args,
    )?)?)
}

fn local_ref(ws: &Workspace, runner: &Runner, reference: &str) -> Result<Option<String>> {
    let output = git(
        ws,
        runner,
        &[
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            reference,
        ],
    )?;
    Ok(output
        .lines()
        .filter_map(|line| line.split_once(' '))
        .find(|(_, name)| *name == reference)
        .map(|(sha, _)| sha.to_owned()))
}

struct Worktree {
    path: PathBuf,
    head: Option<String>,
    branch: Option<String>,
    detached: bool,
    locked: bool,
}

fn worktrees(ws: &Workspace, runner: &Runner) -> Result<Vec<Worktree>> {
    let output = git(ws, runner, &["worktree", "list", "--porcelain", "-z"])?;
    output
        .split("\0\0")
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let path = entry
                .split('\0')
                .find_map(|line| line.strip_prefix("worktree "))
                .context("Git 未返回 Worktree 路径")?;
            Ok(Worktree {
                path: PathBuf::from(path),
                head: entry
                    .split('\0')
                    .find_map(|line| line.strip_prefix("HEAD "))
                    .map(str::to_owned),
                branch: entry
                    .split('\0')
                    .find_map(|line| line.strip_prefix("branch "))
                    .map(str::to_owned),
                detached: entry.split('\0').any(|line| line == "detached"),
                locked: entry
                    .split('\0')
                    .any(|line| line == "locked" || line.starts_with("locked ")),
            })
        })
        .collect()
}

fn clean(ws: &Workspace, runner: &Runner, path: &str, include_ignored: bool) -> Result<()> {
    let mut args = vec![
        "-C",
        path,
        "status",
        "--porcelain",
        "--untracked-files=all",
        "--ignore-submodules=none",
    ];
    if include_ignored {
        args.push("--ignored=matching");
    }
    ensure!(
        git(ws, runner, &args)?.is_empty(),
        "有未提交或未跟踪的文件，已保留 {path}"
    );
    Ok(())
}

fn sync(ws: &Workspace, runner: &Runner, target: &Target, merged: &str) -> Result<String> {
    let branch = format!("refs/heads/{}", target.base);
    let tracking = format!("refs/remotes/origin/{}", target.base);
    git(
        ws,
        runner,
        &[
            "fetch",
            "--no-tags",
            "--no-write-fetch-head",
            "origin",
            &format!("+{branch}:{tracking}"),
        ],
    )?;
    let tip = commit(&ws.root, &tracking)?;
    git(ws, runner, &["merge-base", "--is-ancestor", merged, &tip])
        .context("远端目标分支已不包含该合并提交")?;

    let local = local_ref(ws, runner, &branch)?;
    if let Some(old) = &local {
        if old == &tip {
            return Ok(format!("{} 已同步到 {tip}", target.base));
        }
        git(ws, runner, &["merge-base", "--is-ancestor", old, &tip])
            .context("本地目标分支包含独有提交，请先处理分支差异")?;
    }

    let checked_out = worktrees(ws, runner)?;
    if let Some(tree) = checked_out
        .iter()
        .find(|tree| tree.branch.as_deref() == Some(&branch))
    {
        ensure!(
            checked_out
                .iter()
                .filter(|entry| entry.branch.as_deref() == Some(&branch))
                .count()
                == 1,
            "目标分支在多个 Worktree 中被检出，请先处理"
        );
        let path = tree.path.to_str().context("目标 Worktree 路径无效")?;
        ensure!(!tree.locked, "目标 Worktree 已锁定：{path}");
        clean(ws, runner, path, false)?;
        ensure!(
            git(ws, runner, &["-C", path, "symbolic-ref", "HEAD"])?.trim_end() == branch,
            "目标 Worktree 当前已切换分支"
        );
        git(ws, runner, &["-C", path, "merge", "--ff-only", &tip])?;
    } else if local.is_none() {
        git(
            ws,
            runner,
            &["branch", "--no-track", "--", &target.base, &tip],
        )?;
    } else {
        // A local fetch does its own fast-forward and checked-out branch checks.
        git(
            ws,
            runner,
            &[
                "fetch",
                "--no-tags",
                "--no-write-fetch-head",
                ".",
                &format!("{tip}:{branch}"),
            ],
        )?;
    }
    ensure!(
        local_ref(ws, runner, &branch)?.as_deref() == Some(&tip),
        "本地目标分支在同步期间变化"
    );
    Ok(format!("{} 已快进同步到 {tip}", target.base))
}

fn candidate_branch(ws: &Workspace, runner: &Runner, target: &Target) -> Result<String> {
    let branch = format!("refs/heads/{}", target.head);
    let tracking = format!("refs/remotes/origin/{}", target.head);
    ensure!(
        local_ref(ws, runner, &tracking)?
            .as_deref()
            .is_none_or(|sha| sha == target.candidate),
        "本地候选追踪引用已指向其他提交，请核对后重试"
    );
    let note = if let Some(sha) = publish::remote_sha(ws, runner, &branch)? {
        ensure!(
            sha == target.candidate,
            "远端候选分支已指向其他提交，已保留"
        );
        git(
            ws,
            runner,
            &[
                "push",
                "--porcelain",
                "--no-follow-tags",
                "--recurse-submodules=no",
                &format!("--force-with-lease={branch}:{}", target.candidate),
                "origin",
                &format!(":{branch}"),
            ],
        )?;
        "已删除远端候选分支"
    } else {
        "远端候选分支已不存在"
    };
    ensure!(
        publish::remote_sha(ws, runner, &branch)?.is_none(),
        "远端候选分支仍存在，请刷新核对"
    );
    if let Some(sha) = local_ref(ws, runner, &tracking)? {
        ensure!(
            sha == target.candidate,
            "已清理远端；本地候选追踪引用指向其他提交，已保留"
        );
        git(
            ws,
            runner,
            &["update-ref", "-d", &tracking, &target.candidate],
        )?;
    }
    let detail = if local_ref(ws, runner, &branch)?.is_some() {
        format!("{note}；用户本地同名分支仍可继续使用")
    } else {
        note.to_owned()
    };
    Ok(detail)
}

fn temporary(
    ws: &Workspace,
    runner: &Runner,
    target: &Target,
    attempt: &Attempt,
) -> Result<String> {
    let record = status(&ws.data, &attempt.id)?;
    ensure!(
        record["task"] == target.task && record["spec_sha"] == target.spec,
        "Run 归属与接力不一致"
    );
    let Some(path) = record["worktree"].as_str() else {
        return Ok("此 Run 未创建临时 Worktree".into());
    };
    let path = Path::new(path);
    ensure!(
        path.is_absolute()
            && path
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|name| name.starts_with("aep-work-")),
        "工作区归属无法确认"
    );
    let exists = path.try_exists()?;
    let trees = worktrees(ws, runner)?;
    let canonical = if exists {
        Some(path.canonicalize()?)
    } else {
        None
    };
    let registered = trees.iter().find(|tree| {
        tree.path == path
            || canonical
                .as_ref()
                .is_some_and(|expected| tree.path.canonicalize().ok().as_ref() == Some(expected))
    });
    let Some(tree) = registered else {
        ensure!(!exists, "未找到该 Run 的 Git Worktree，目录已保留");
        return Ok("临时 Worktree 已清理".into());
    };
    ensure!(
        matches!(
            record["status"].as_str(),
            Some("failed" | "awaiting_human_review")
        ) && record["finished_at"].as_str().is_some(),
        "Run 尚未确认结束，已保留工作区"
    );
    if let Some(canonical) = canonical {
        ensure!(
            fs::symlink_metadata(path)?.is_dir(),
            "工作区路径发生变化，已保留"
        );
        ensure!(
            canonical != ws.root.canonicalize()?,
            "当前 Studio 工作区已保留"
        );
    }
    ensure!(
        tree.detached && !tree.locked,
        "临时 Worktree 已切换分支或被锁定，已保留"
    );
    let expected = record["candidate_sha"]
        .as_str()
        .or_else(|| record["requested_candidate_sha"].as_str())
        .or_else(|| record["spec_sha"].as_str())
        .context("缺少 Worktree 的候选 SHA")?;
    ensure!(
        tree.head.as_deref() == Some(expected)
            && (!exists || commit(&tree.path, "HEAD")? == expected),
        "临时 Worktree 已有新的提交，已保留"
    );
    if record["candidate_sha"].is_string() {
        ensure!(
            commit(&ws.root, &format!("refs/aep/runs/{}", attempt.id))? == expected,
            "归档候选引用与 Worktree 不一致，已保留"
        );
    }
    let path = tree.path.to_str().context("临时 Worktree 路径无效")?;
    if exists {
        clean(ws, runner, path, true)?;
    }
    git(ws, runner, &["worktree", "remove", "--", path])?;
    Ok(format!("已移除临时 Worktree {path}"))
}

pub(crate) fn finish(
    ws: &Workspace,
    runner: &Runner,
    publication: &Publication,
) -> Result<Receipt> {
    let target = &publication.target;
    ident(&target.run, 64)?;
    ident(&target.task, 63)?;
    let path = ws.data.join("runs").join(&target.run);
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.join("cleanup.lock"))?;
    lock.try_lock().context("此接力正在收尾")?;
    let number = publication.pr.as_ref().context("缺少已发布的 PR")?.number;
    ensure!(
        publish::repository(ws, runner)? == target.repo,
        "origin 已更换，请核对 GitHub 仓库"
    );
    ensure!(
        target.head == format!("aep/pr/{}/{}", target.task, target.run)
            && target.head != target.base,
        "候选分支与 AEP 发布记录不符"
    );
    git(
        ws,
        runner,
        &["check-ref-format", &format!("refs/heads/{}", target.base)],
    )?;
    let pr = github_pr::load(ws, runner, target, number)?;
    ensure!(
        pr.state == "MERGED"
            && pr.head == target.candidate
            && pr.head_ref == target.head
            && pr.base_ref == target.base
            && !pr.merged_sha.is_empty(),
        "GitHub 合并记录与已发布候选不一致"
    );
    let relay = relay::load_one(ws, &target.relay)?;
    ensure!(
        relay
            .latest(Round::Second)
            .is_some_and(|attempt| attempt.id == target.run),
        "此 PR 不是当前接力的第二轮"
    );
    for round in [Round::First, Round::Second] {
        let run = &relay.latest(round).context("缺少接力执行证据")?.id;
        let evidence = status(&ws.data, run)?;
        ensure!(
            evidence["candidate_sha"] == commit(&ws.root, &format!("refs/aep/runs/{run}"))?,
            "Run {run} 的证据与归档候选不一致"
        );
    }
    let target_step = Step::from(sync(ws, runner, target, &pr.merged_sha));
    let (branch, worktrees) = if target_step.done {
        let branch = Step::from(candidate_branch(ws, runner, target));
        let worktrees = relay
            .attempts
            .iter()
            .filter(|attempt| {
                ws.data
                    .join("runs")
                    .join(&attempt.id)
                    .join("evidence.json")
                    .is_file()
            })
            .map(|attempt| Temporary {
                run: attempt.id.clone(),
                step: Step::from(temporary(ws, runner, target, attempt)),
            })
            .collect();
        (Some(branch), worktrees)
    } else {
        (None, Vec::new())
    };
    let receipt = Receipt {
        pr: number,
        candidate: target.candidate.clone(),
        base: target.base.clone(),
        merged_sha: pr.merged_sha,
        merged_by: pr.merged_by,
        merged_at: pr.merged_at,
        finished_at_unix_seconds: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        target: target_step,
        branch,
        worktrees,
    };
    save(&path, &receipt)?;
    Ok(receipt)
}
