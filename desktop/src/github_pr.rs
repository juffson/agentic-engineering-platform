use crate::{
    Runner,
    publish::{Publication, Target, field, github, repository},
};
use aep::workspace::Workspace;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashSet,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    path::Path,
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Outcome {
    Pending,
    Passed,
    Skipped,
    Failed,
}

#[derive(Clone)]
pub(crate) struct Check {
    pub id: String,
    pub number: Option<u64>,
    pub name: String,
    pub workflow: String,
    pub status: String,
    pub conclusion: String,
    pub outcome: Outcome,
    pub url: String,
    pub description: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Note {
    id: String,
    pub author: String,
    pub kind: String,
    pub date: String,
    pub body: String,
    pub location: Option<String>,
    pub url: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct MergeReceipt {
    pub number: u64,
    pub head: String,
    pub base: String,
    pub confirmed_by: String,
    pub confirmed_at_unix_seconds: u64,
    pub merged_sha: Option<String>,
}

pub(crate) struct Snapshot {
    pub title: String,
    pub state: String,
    pub draft: bool,
    pub head: String,
    pub base: String,
    pub base_ref: String,
    pub head_ref: String,
    pub mergeable: String,
    pub merge_state: String,
    pub review_decision: String,
    pub method: Option<&'static str>,
    permission: bool,
    pub checks: Vec<Check>,
    pub notes: Vec<Note>,
    pub more_notes: bool,
    pub merged_at: String,
    pub merged_by: String,
    pub merged_sha: String,
    pub receipt: Option<MergeReceipt>,
}

impl Snapshot {
    pub fn blocker(&self, target: &Target) -> Option<&'static str> {
        if self.state == "MERGED" {
            Some("此 PR 已合并")
        } else if self
            .receipt
            .as_ref()
            .is_some_and(|receipt| receipt.merged_sha.is_some())
        {
            Some("GitHub 已受理合并，正在同步结果")
        } else if self.state != "OPEN" || self.draft {
            Some("PR 已关闭或仍为草稿")
        } else if self.head != target.candidate
            || self.base_ref != target.base
            || self.head_ref != target.head
        {
            Some("PR 的分支或候选 SHA 已变化，请核对发布证据")
        } else if self
            .checks
            .iter()
            .any(|check| check.outcome == Outcome::Failed)
        {
            Some("GitHub 检查存在失败")
        } else if self
            .checks
            .iter()
            .any(|check| check.outcome == Outcome::Pending)
        {
            Some("GitHub 检查仍在运行")
        } else if !self
            .checks
            .iter()
            .any(|check| check.outcome == Outcome::Passed)
        {
            Some("尚未收到通过的 GitHub 检查")
        } else if matches!(
            self.review_decision.as_str(),
            "REVIEW_REQUIRED" | "CHANGES_REQUESTED"
        ) || self.changes_requested()
        {
            Some("GitHub 仍要求审查或修改")
        } else if self.mergeable != "MERGEABLE"
            || !matches!(self.merge_state.as_str(), "CLEAN" | "HAS_HOOKS")
        {
            Some("GitHub 尚未允许合并，请查看分支状态或冲突")
        } else if !self.permission || self.method.is_none() {
            Some("当前 GitHub 账号或仓库设置不允许此操作")
        } else {
            None
        }
    }

    fn changes_requested(&self) -> bool {
        let mut authors = HashSet::new();
        self.notes
            .iter()
            .filter(|note| {
                matches!(
                    note.kind.as_str(),
                    "CHANGES_REQUESTED" | "APPROVED" | "DISMISSED"
                )
            })
            .any(|note| authors.insert(note.author.as_str()) && note.kind == "CHANGES_REQUESTED")
    }

    pub fn matches_confirmation(&self, previous: &Self) -> bool {
        self.head == previous.head
            && self.base == previous.base
            && self.method == previous.method
            && self.state == previous.state
            && self.draft == previous.draft
            && self.mergeable == previous.mergeable
            && self.merge_state == previous.merge_state
            && self.review_decision == previous.review_decision
            && self.notes == previous.notes
            && self.permission == previous.permission
            && self.checks.len() == previous.checks.len()
            && self
                .checks
                .iter()
                .zip(&previous.checks)
                .all(|(a, b)| a.id == b.id && a.status == b.status && a.conclusion == b.conclusion)
    }
}

fn text(value: &Value, field: &str) -> String {
    value[field].as_str().unwrap_or_default().to_owned()
}

fn check(value: &Value) -> Result<Check> {
    let run = value["__typename"] == "CheckRun";
    let status = text(value, if run { "status" } else { "state" });
    let conclusion = if run {
        text(value, "conclusion")
    } else {
        status.clone()
    };
    let outcome = if run && status != "COMPLETED" {
        Outcome::Pending
    } else {
        match conclusion.as_str() {
            "SUCCESS" => Outcome::Passed,
            "SKIPPED" | "NEUTRAL" => Outcome::Skipped,
            "PENDING" | "EXPECTED" | "" => Outcome::Pending,
            _ => Outcome::Failed,
        }
    };
    Ok(Check {
        id: field(value, "id")?.into(),
        number: value["databaseId"].as_u64(),
        name: text(value, if run { "name" } else { "context" }),
        workflow: text(&value["checkSuite"]["workflowRun"]["workflow"], "name"),
        status,
        conclusion,
        outcome,
        url: text(value, if run { "detailsUrl" } else { "targetUrl" }),
        description: text(value, "description"),
    })
}

fn discussion(pr: &Value) -> (Vec<Note>, bool) {
    let mut notes = Vec::new();
    let mut record = |connection: &Value, kind: &str, location: Option<String>| {
        let entries = connection["nodes"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        notes.extend(
            entries
                .iter()
                .filter(|item| item["state"] != "PENDING")
                .map(|item| Note {
                    id: text(item, "id"),
                    author: text(&item["author"], "login"),
                    kind: item["state"].as_str().unwrap_or(kind).to_owned(),
                    date: item["submittedAt"]
                        .as_str()
                        .or_else(|| item["createdAt"].as_str())
                        .unwrap_or_default()
                        .into(),
                    body: text(item, "body"),
                    location: location.clone(),
                    url: text(item, "url"),
                }),
        );
        connection["totalCount"].as_u64().unwrap_or(0) > entries.len() as u64
    };
    let mut more =
        record(&pr["reviews"], "COMMENTED", None) | record(&pr["comments"], "COMMENTED", None);
    if let Some(threads) = pr["reviewThreads"]["nodes"].as_array() {
        more |= pr["reviewThreads"]["totalCount"].as_u64().unwrap_or(0) > threads.len() as u64;
        for thread in threads {
            let path = text(thread, "path");
            let location = thread["line"]
                .as_u64()
                .map_or(path.clone(), |line| format!("{path}:{line}"));
            let kind = if thread["isResolved"] == true {
                "RESOLVED"
            } else {
                "INLINE"
            };
            more |= record(&thread["comments"], kind, Some(location));
        }
    }
    notes.sort_by(|a, b| (&b.date, &b.id).cmp(&(&a.date, &a.id)));
    (notes, more)
}

pub(crate) fn read_receipt(path: &Path) -> Result<Option<MergeReceipt>> {
    match File::open(path.join("merge.json")) {
        Ok(file) => Ok(Some(serde_json::from_reader(file)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn load(
    ws: &Workspace,
    runner: &Runner,
    target: &Target,
    number: u64,
) -> Result<Snapshot> {
    let (owner, repo) = target.repo.split_once('/').context("GitHub 仓库地址无效")?;
    let args = [
        "graphql",
        "-f",
        concat!("query=", include_str!("github-pr.graphql")),
        "-f",
        &format!("owner={owner}"),
        "-f",
        &format!("name={repo}"),
        "-F",
        &format!("number={number}"),
    ];
    let mut checks = Vec::new();
    let mut cursor = None;
    let mut head = None;
    let mut base = None;
    let repository = loop {
        let mut query = args.to_vec();
        let after;
        if let Some(cursor) = cursor.as_ref() {
            after = format!("cursor={cursor}");
            query.extend(["-f", &after]);
        }
        let result = github(ws, runner, &query)?;
        let repo = &result["data"]["repository"];
        let pr = &repo["pullRequest"];
        let oid = field(pr, "headRefOid").context("无法读取 GitHub PR")?;
        let base_oid = field(pr, "baseRefOid")?;
        ensure!(
            head.as_deref().is_none_or(|head| head == oid)
                && base.as_deref().is_none_or(|base| base == base_oid),
            "读取检查期间 PR 的提交发生变化，请刷新"
        );
        head = Some(oid.to_owned());
        base = Some(base_oid.to_owned());
        let commit = &pr["commits"]["nodes"][0]["commit"];
        let contexts = &commit["statusCheckRollup"]["contexts"];
        if !contexts.is_null() {
            ensure!(commit["oid"] == oid, "检查所用的提交与 PR head 不一致");
            for node in contexts["nodes"]
                .as_array()
                .context("GitHub 检查列表无效")?
            {
                checks.push(check(node)?);
            }
        }
        if contexts["pageInfo"]["hasNextPage"] != true {
            break repo.clone();
        }
        let next = field(&contexts["pageInfo"], "endCursor")?.to_owned();
        ensure!(cursor.as_ref() != Some(&next), "GitHub 检查分页未推进");
        cursor = Some(next);
    };
    checks.sort_by(|a, b| (&a.workflow, &a.name, &a.id).cmp(&(&b.workflow, &b.name, &b.id)));
    let pr = &repository["pullRequest"];
    let (notes, more_notes) = discussion(pr);
    let allowed = [
        ("mergeCommitAllowed", "merge"),
        ("squashMergeAllowed", "squash"),
        ("rebaseMergeAllowed", "rebase"),
    ];
    Ok(Snapshot {
        title: text(pr, "title"),
        state: text(pr, "state"),
        draft: pr["isDraft"] == true,
        head: field(pr, "headRefOid")?.into(),
        base: field(pr, "baseRefOid")?.into(),
        base_ref: field(pr, "baseRefName")?.into(),
        head_ref: text(pr, "headRefName"),
        mergeable: text(pr, "mergeable"),
        merge_state: text(pr, "mergeStateStatus"),
        review_decision: text(pr, "reviewDecision"),
        method: allowed
            .into_iter()
            .find_map(|(key, name)| (repository[key] == true).then_some(name)),
        permission: matches!(
            repository["viewerPermission"].as_str(),
            Some("ADMIN" | "MAINTAIN" | "WRITE")
        ),
        checks,
        notes,
        more_notes,
        merged_at: text(pr, "mergedAt"),
        merged_by: text(&pr["mergedBy"], "login"),
        merged_sha: text(&pr["mergeCommit"], "oid"),
        receipt: read_receipt(&ws.data.join("runs").join(&target.run))?,
    })
}

pub(crate) fn check_details(
    ws: &Workspace,
    runner: &Runner,
    target: &Target,
    check: &Check,
) -> Result<String> {
    let Some(id) = check.number else {
        return Ok(check.description.clone());
    };
    let endpoint = format!("repos/{}/check-runs/{id}", target.repo);
    let metadata = github(ws, runner, &[&endpoint])?;
    let output = &metadata["output"];
    let mut details = [
        text(output, "title"),
        text(output, "summary"),
        text(output, "text"),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>();
    if let Ok(annotations) = github(
        ws,
        runner,
        &[&format!("{endpoint}/annotations?per_page=30")],
    ) && let Some(items) = annotations.as_array()
    {
        details.extend(items.iter().map(|item| {
            format!(
                "{}:{} · {}\n{}",
                text(item, "path"),
                item["start_line"],
                text(item, "title"),
                text(item, "message")
            )
        }));
    }
    // Actions jobs use the check run's numeric ID in their details URL.
    if check
        .url
        .strip_prefix(&format!("https://github.com/{}/actions/runs/", target.repo))
        .and_then(|url| url.split_once("/job/"))
        .and_then(|(_, job)| job.parse::<u64>().ok())
        == Some(id)
        && let Ok(log) = actions_log(ws, runner, &target.repo, id)
        && !log.trim().is_empty()
    {
        details.push(format!("Actions 日志末尾\n{log}"));
    }
    Ok(details.join("\n\n"))
}

fn actions_log(ws: &Workspace, runner: &Runner, repo: &str, id: u64) -> Result<String> {
    const LIMIT: u64 = 32 * 1024;
    let mut file = tempfile::tempfile()?;
    // Store stdout on disk so a large job log does not accumulate in memory.
    let result = Command::new("gh")
        .current_dir(&ws.root)
        .env("PATH", runner.path()?)
        .env("GH_PROMPT_DISABLED", "1")
        .args([
            "api",
            "--hostname",
            "github.com",
            &format!("repos/{repo}/actions/jobs/{id}/logs"),
        ])
        .stdout(file.try_clone()?)
        .stderr(Stdio::null())
        .status()?;
    ensure!(result.success(), "Actions 日志暂不可用");
    file.seek(SeekFrom::Start(
        file.metadata()?.len().saturating_sub(LIMIT),
    ))?;
    let mut bytes = Vec::new();
    file.take(LIMIT).read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn save_receipt(path: &Path, receipt: &MergeReceipt) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(path)?;
    serde_json::to_writer_pretty(&mut file, receipt)?;
    file.persist(path.join("merge.json"))?;
    Ok(())
}

pub(crate) fn merge(
    ws: &Workspace,
    runner: &Runner,
    publication: &Publication,
    seen: &Snapshot,
) -> Result<Snapshot> {
    let target = &publication.target;
    let number = publication.pr.as_ref().context("缺少已发布的 PR")?.number;
    let path = ws.data.join("runs").join(&target.run);
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.join("merge.lock"))?;
    lock.try_lock().context("此 PR 正在合并")?;
    ensure!(
        repository(ws, runner)? == target.repo,
        "origin 已更换，请核对 GitHub 仓库"
    );
    let current = load(ws, runner, target, number)?;
    if current.state == "MERGED" {
        return Ok(current);
    }
    ensure!(
        seen.matches_confirmation(&current),
        "PR 提交、检查或审查意见已变化，请刷新后重新确认"
    );
    if let Some(reason) = current.blocker(target) {
        anyhow::bail!("{reason}");
    }
    if current.base != target.base_sha {
        let compare = github(
            ws,
            runner,
            &[&format!(
                "repos/{}/compare/{}...{}?per_page=1",
                target.repo, current.base, current.head
            )],
        )?;
        ensure!(
            compare["merge_base_commit"]["sha"] == target.merge_base,
            "GitHub 的比较基点已变化，请在 GitHub 重新审查"
        );
    }
    let user = github(ws, runner, &["user"])?;
    let mut receipt = MergeReceipt {
        number,
        head: current.head,
        base: current.base,
        confirmed_by: field(&user, "login")?.into(),
        confirmed_at_unix_seconds: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        merged_sha: None,
    };
    save_receipt(&path, &receipt)?;
    let response = github(
        ws,
        runner,
        &[
            &format!("repos/{}/pulls/{number}/merge", target.repo),
            "--method",
            "PUT",
            "-f",
            &format!("sha={}", receipt.head),
            "-f",
            &format!(
                "merge_method={}",
                current.method.context("仓库未启用合并方式")?
            ),
        ],
    );
    match response {
        Ok(response) => {
            ensure!(
                response["merged"] == true,
                "{}",
                response["message"].as_str().unwrap_or("GitHub 拒绝合并")
            );
            receipt.merged_sha = Some(field(&response, "sha")?.into());
            save_receipt(&path, &receipt)?;
        }
        Err(error) => {
            // The server may have committed the merge before the client lost its response.
            if let Ok(mut snapshot) = load(ws, runner, target, number)
                && snapshot.state == "MERGED"
            {
                if snapshot.merged_by == receipt.confirmed_by && !snapshot.merged_sha.is_empty() {
                    receipt.merged_sha = Some(snapshot.merged_sha.clone());
                    save_receipt(&path, &receipt)?;
                    snapshot.receipt = Some(receipt);
                }
                return Ok(snapshot);
            }
            return Err(error);
        }
    }
    load(ws, runner, target, number)
}
