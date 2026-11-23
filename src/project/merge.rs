//! Local, reviewed fast-forwards of sealed item deliveries. No remote commands.
use super::{Library, archive::Delivery};
use crate::workspace::{Workspace, commit, git, ident, read};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

const DIFF_LIMIT: usize = 96 * 1024;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeStatus {
    Pending,
    Merged,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeReceipt {
    pub status: MergeStatus,
    pub delivery_run: String,
    pub repository: PathBuf,
    pub target_branch: String,
    pub before_sha: String,
    pub candidate_sha: String,
    pub merged_sha: Option<String>,
    pub mode: String,
    pub diff_sha256: String,
    pub files_changed: usize,
    pub insertions: u64,
    pub deletions: u64,
    pub intent_at: String,
    pub merged_at: Option<String>,
    pub operator: String,
    pub recovered: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recovery {
    Record,
    Retry,
    Changed,
}
impl Recovery {
    pub fn message(self) -> &'static str {
        match self {
            Self::Record => "分支已到已验收提交，可以恢复补记回执",
            Self::Retry => "合并未完成，可以重新确认重试",
            Self::Changed => "目标分支在合并过程中被修改，请放弃意图后重新预览",
        }
    }
}
impl MergeReceipt {
    pub fn recovery(&self, tip: &str) -> Recovery {
        if tip == self.candidate_sha && self.mode != "already_contained" {
            Recovery::Record
        } else if tip == self.before_sha {
            Recovery::Retry
        } else {
            Recovery::Changed
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AbandonedIntent {
    pub intent: MergeReceipt,
    pub at: String,
    pub reason: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct Candidate {
    pub repository: PathBuf,
    pub sha: String,
    pub spec_sha: Option<String>,
    pub transfer_run: String,
    pub default_target: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Branch {
    pub name: String,
    pub tip: String,
    pub checked_out: Option<PathBuf>,
    pub status: String,
    pub reason: String,
    pub internal: bool,
    pub merge_base: Option<String>,
}
impl Branch {
    pub fn writable(&self) -> bool {
        self.status == "fast_forward" || self.status == "already_contained"
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Preview {
    pub candidate: Candidate,
    pub branch: Branch,
    pub diff: String,
    pub stat: String,
    pub truncated: bool,
    pub diff_sha256: String,
    pub files_changed: usize,
    pub insertions: u64,
    pub deletions: u64,
    pub same_as_accepted: bool,
}
fn sha(value: &str) -> Result<()> {
    ensure!(
        matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit()),
        "无效的提交 SHA"
    );
    Ok(())
}
fn branch_ref(name: &str, repo: &Path) -> Result<String> {
    ensure!(!name.is_empty() && !name.starts_with('-'), "无效的本地分支");
    let reference = format!("refs/heads/{name}");
    git(repo, &["check-ref-format", &reference]).context("无效的本地分支")?;
    Ok(reference)
}
fn archived(home: &Path, d: &Delivery, index: usize, run: &str) -> Result<serde_json::Value> {
    ident(run, 64)?;
    read(
        &d.directory(home)
            .join(super::archive::run_directory(index, run))
            .join("evidence.json"),
    )
}
/// Follow the *sealed* consumer chain, never the mutable current stage selection.
pub fn candidate(home: &Path, d: &Delivery) -> Result<Candidate> {
    let history = d.snapshot(home)?;
    let mut run = d.run.clone();
    let mut seen = BTreeSet::new();
    let mut downstream = Vec::new();
    loop {
        ensure!(seen.insert(run.clone()), "归档交接链存在循环");
        let matches: Vec<_> = history
            .rounds
            .iter()
            .enumerate()
            .filter(|(_, r)| r.run == run)
            .collect();
        ensure!(matches.len() == 1, "归档交接链缺失或执行不唯一：{run}");
        let (index, row) = matches[0];
        let ev = archived(home, d, index, &run)?;
        ensure!(ev["run_id"] == run, "归档执行回执不匹配");
        let t = row
            .read_stage_transfer
            .as_ref()
            .context("归档交接链没有已验收提交")?;
        ensure!(
            t.project == d.project
                && t.work == d.work
                && t.from.checked_add(1) == Some(t.to)
                && row.stage == Some(t.to)
                && ev["read_stage_transfer"] == serde_json::to_value(t)?
                && history.stage_transfers.iter().filter(|v| *v == t).count() == 1,
            "归档交接链不完整或不匹配"
        );
        if run == d.run {
            ensure!(t.run == d.accepted_run, "交付未消费归档的独立验收");
        }
        ensure!(
            row.status == "succeeded" && ev["status"] == "succeeded",
            "后续阶段尚未通过"
        );
        downstream.push(ev);
        if let Some(value) = &t.candidate {
            sha(value)?;
            ident(&t.run, 64)?;
            // A reopened item can retain older candidates at the same stage. Only the
            // exact sealed consumer chain selects this delivery's candidate.
            let sources: Vec<_> = history
                .rounds
                .iter()
                .enumerate()
                .filter(|(_, r)| r.run == t.run)
                .collect();
            ensure!(
                sources.len() == 1
                    && sources[0].1.round == Some(2)
                    && sources[0].1.stage == Some(t.from)
                    && sources[0].1.repository == t.repository,
                "缺少开发第二轮归档回执"
            );
            let source = archived(home, d, sources[0].0, &t.run)?;
            ensure!(
                source["run_id"] == t.run
                    && source["candidate_sha"] == *value
                    && source["check_exit_code"] == 0,
                "已验收提交与归档接力回执不一致"
            );
            let ws = Workspace::open(t.repository.as_deref().context("没有本地仓库")?)
                .context("仓库不可用")?;
            ensure!(
                commit(&ws.root, value).context("已验收提交不可用")? == *value
                    && commit(&ws.root, &format!("refs/aep/runs/{}", t.run))
                        .context("已验收提交引用已改变")?
                        == *value,
                "已验收提交引用已改变"
            );
            for ev in downstream {
                let path = ev["workspace"]
                    .as_str()
                    .filter(|p| !p.is_empty())
                    .context("后续阶段回执缺少工作区，无法校验验收后的改动")?;
                let path = Path::new(path);
                ensure!(path.is_dir(), "后续阶段工作区不可用：{}", path.display());
                let stage_ws = Workspace::open(path)?;
                ensure!(
                    stage_ws.data == ws.data,
                    "后续阶段工作区不属于交付仓库：{}",
                    path.display()
                );
                let dirty = git(path, &["status", "--porcelain", "--untracked-files=all"])?;
                let head = commit(path, "HEAD")?;
                let changes = git(path, &["diff", "--name-only", value, &head, "--"])?;
                ensure!(
                    head == *value && dirty.is_empty(),
                    "验收后有未纳入的改动：{}\n{}{}",
                    path.display(),
                    changes,
                    dirty
                );
            }
            let spec_sha = source["spec_sha"].as_str().map(str::to_owned);
            if let Some(spec) = &spec_sha {
                sha(spec)?;
                commit(&ws.root, spec)?;
            }
            let main_target = worktrees(&ws.root)?
                .first()
                .and_then(|(_, b)| b.as_deref())
                .and_then(|b| b.strip_prefix("refs/heads/"))
                .map(str::to_owned);
            return Ok(Candidate {
                repository: ws.root,
                sha: value.clone(),
                spec_sha,
                transfer_run: t.run.clone(),
                default_target: source["request"]["target"]
                    .as_str()
                    .or(source["target"].as_str())
                    .map(str::to_owned)
                    .or(main_target),
            });
        }
        run = t.run.clone();
    }
}
fn ancestor(repo: &Path, a: &str, b: &str) -> Result<bool> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["merge-base", "--is-ancestor", a, b])
        .output()?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!("无法校验分支祖先：{}", String::from_utf8_lossy(&out.stderr)),
    }
}
fn worktrees(repo: &Path) -> Result<Vec<(PathBuf, Option<String>)>> {
    let raw = git(repo, &["worktree", "list", "--porcelain", "-z"])?;
    let mut rows = Vec::new();
    let mut path = None;
    let mut branch = None;
    for field in raw.split('\0') {
        if let Some(p) = field.strip_prefix("worktree ") {
            path = Some(PathBuf::from(p));
        } else if let Some(b) = field.strip_prefix("branch ") {
            branch = Some(b.to_owned());
        } else if field.is_empty()
            && let Some(p) = path.take()
        {
            rows.push((p, branch.take()));
        }
    }
    Ok(rows)
}
pub fn branches(c: &Candidate) -> Result<Vec<Branch>> {
    let trees = worktrees(&c.repository)?;
    let main = trees.first().context("缺少主工作区")?.0.canonicalize()?;
    let refs = git(
        &c.repository,
        &["for-each-ref", "--format=%(refname:strip=2)", "refs/heads/"],
    )?;
    refs.lines()
        .map(|name| {
            let reference = branch_ref(name, &c.repository)?;
            let tip = commit(&c.repository, &reference)?;
            let checked_out = trees
                .iter()
                .filter(|(_, b)| b.as_deref() == Some(&reference))
                // A branch can be force-checked-out more than once; any linked checkout blocks it.
                .min_by_key(|(p, _)| p.canonicalize().is_ok_and(|p| p == main))
                .map(|(p, _)| p.clone());
            let mut b = Branch {
                name: name.into(),
                tip: tip.clone(),
                checked_out,
                status: String::new(),
                reason: String::new(),
                internal: name.starts_with("aep-stage/") || name.starts_with("aep-task/"),
                merge_base: None,
            };
            if ancestor(&c.repository, &c.sha, &tip)? {
                b.status = "already_contained".into();
                b.reason = "已包含已验收提交".into();
            } else if ancestor(&c.repository, &tip, &c.sha)? {
                b.status = "fast_forward".into();
                b.reason = "可快进".into();
            } else {
                b.status = "diverged".into();
                b.reason = "无法快进：分支已分叉".into();
                let out = Command::new("git")
                    .arg("-C")
                    .arg(&c.repository)
                    .args(["merge-base", &tip, &c.sha])
                    .output()?;
                match out.status.code() {
                    Some(0) => b.merge_base = Some(String::from_utf8(out.stdout)?.trim().into()),
                    Some(1) => b.reason = "无法快进：没有共同祖先".into(),
                    _ => bail!("无法读取分叉点：{}", String::from_utf8_lossy(&out.stderr)),
                }
            }
            if let Some(path) = &b.checked_out {
                if !path.canonicalize().is_ok_and(|p| p == main) {
                    b.status = "unwritable".into();
                    b.reason = format!("不可写入：在其他工作树检出 {}", path.display());
                } else if !git(path, &["status", "--porcelain", "--untracked-files=all"])?
                    .is_empty()
                {
                    b.status = "unwritable".into();
                    b.reason = format!("不可写入：主工作区不干净 {}", path.display());
                }
            }
            Ok(b)
        })
        .collect()
}
fn diff(repo: &Path, from: &str, to: &str, kind: &str) -> Result<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            kind,
            from,
            to,
            "--",
        ])
        .output()?;
    ensure!(out.status.success(), "无法读取最终 Diff");
    Ok(out.stdout)
}
pub fn preview(home: &Path, d: &Delivery, target: &str) -> Result<Preview> {
    let c = candidate(home, d)?;
    branch_ref(target, &c.repository)?;
    let branch = branches(&c)?
        .into_iter()
        .find(|b| b.name == target)
        .context("本地目标分支不存在")?;
    let patch = diff(&c.repository, &branch.tip, &c.sha, "--patch")?;
    let mut hash = Sha256::new();
    for field in [&c.sha, &branch.name, &branch.tip] {
        hash.update(field.as_bytes());
        hash.update([0]);
    }
    hash.update(&patch);
    let nums = String::from_utf8(diff(&c.repository, &branch.tip, &c.sha, "--numstat")?)?;
    let (mut files_changed, mut insertions, mut deletions) = (0, 0, 0);
    for line in nums.lines() {
        let mut fields = line.split('\t');
        files_changed += 1;
        insertions += fields
            .next()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        deletions += fields
            .next()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
    }
    let same_as_accepted = c
        .spec_sha
        .as_ref()
        .map(|spec| diff(&c.repository, spec, &c.sha, "--patch").map(|p| p == patch))
        .transpose()?
        .unwrap_or(false);
    Ok(Preview {
        stat: String::from_utf8_lossy(&diff(&c.repository, &branch.tip, &c.sha, "--stat")?)
            .into_owned(),
        truncated: patch.len() > DIFF_LIMIT,
        diff: String::from_utf8_lossy(&patch[..patch.len().min(DIFF_LIMIT)]).into_owned(),
        diff_sha256: format!("{:x}", hash.finalize()),
        candidate: c,
        branch,
        files_changed,
        insertions,
        deletions,
        same_as_accepted,
    })
}
/// Full patch is exported only on explicit request; preview remains read-only.
pub fn export_patch(home: &Path, d: &Delivery, target: &str, output: &Path) -> Result<()> {
    let p = preview(home, d, target)?;
    let bytes = diff(
        &p.candidate.repository,
        &p.branch.tip,
        &p.candidate.sha,
        "--binary",
    )?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(())
}
pub fn receipt(home: &Path, d: &Delivery) -> Result<Option<MergeReceipt>> {
    for id in [&d.project, &d.work, &d.run] {
        ident(id, 64)?;
    }
    let path = d.directory(home).join("merge.json");
    match fs::read(path) {
        Ok(bytes) => {
            let r: MergeReceipt = serde_json::from_slice(&bytes).context("合并回执无法读取")?;
            ensure!(
                r.status == MergeStatus::Merged
                    && r.delivery_run == d.run
                    && r.merged_sha.is_some(),
                "合并回执与交付不匹配"
            );
            Ok(Some(r))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
fn delivery_mut<'a>(
    lib: &'a mut Library,
    project: &str,
    work: &str,
    run: &str,
) -> Result<&'a mut Delivery> {
    lib.projects
        .iter_mut()
        .find(|p| p.id == project)
        .and_then(|p| p.work.iter_mut().find(|w| w.id == work))
        .and_then(|w| w.deliveries.iter_mut().find(|d| d.run == run))
        .context("归档交付不存在")
}
fn store(home: &Path, lib: &mut Library, d: &Delivery, r: &MergeReceipt) -> Result<()> {
    lib.change(home, |next| {
        delivery_mut(next, &d.project, &d.work, &d.run)?.merge = Some(r.clone());
        Ok(())
    })
}
/// Both CLI and Studio use this transaction. Pending survives ref updates and process exits.
pub fn merge(
    home: &Path,
    lib: &mut Library,
    project: &str,
    work: &str,
    run: &str,
    target: &str,
    digest: &str,
) -> Result<MergeReceipt> {
    let d = delivery_mut(lib, project, work, run)?.clone();
    if let Some(r) = receipt(home, &d)?
        .or(d.merge.clone())
        .filter(|r| r.status == MergeStatus::Merged)
    {
        ensure!(r.target_branch == target, "此交付已合并到其他目标分支");
        return Ok(r);
    }
    let c = candidate(home, &d)?;
    let ws = Workspace::open(&c.repository)?;
    fs::create_dir_all(&ws.data)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(ws.data.join("local-delivery.lock"))?;
    lock.try_lock()
        .context("此仓库正在执行另一次本地合并，请稍后重试")?;
    // Reload after locking; the catalog has its own optimistic concurrency fence.
    let mut current = Library::open(home)?;
    let fresh = delivery_mut(&mut current, project, work, run)?.clone();
    *lib = current;
    if let Some(r) = receipt(home, &fresh)?
        .or(fresh.merge.clone())
        .filter(|r| r.status == MergeStatus::Merged)
    {
        ensure!(r.target_branch == target, "此交付已合并到其他目标分支");
        return Ok(r);
    }
    let reference = branch_ref(target, &ws.root)?;
    let mut r = if let Some(pending) = fresh.merge.clone() {
        ensure!(
            pending.target_branch == target
                && pending.candidate_sha == c.sha
                && pending.diff_sha256 == digest
                && pending.delivery_run == run
                && pending.repository.canonicalize()? == ws.root.canonicalize()?,
            "存在不同的合并意图，请恢复或放弃后重新查看 Diff"
        );
        pending
    } else {
        let p = preview(home, &fresh, target)?;
        ensure!(p.diff_sha256 == digest, "目标或变更已改变，请重新查看 Diff");
        ensure!(p.branch.writable(), "{}", p.branch.reason);
        let name = git(&ws.root, &["config", "user.name"])?;
        let email = git(&ws.root, &["config", "user.email"])?;
        ensure!(
            !name.trim().is_empty() && !email.trim().is_empty(),
            "请配置 Git 确认人身份"
        );
        let r = MergeReceipt {
            status: MergeStatus::Pending,
            delivery_run: run.into(),
            repository: ws.root.clone(),
            target_branch: target.into(),
            before_sha: p.branch.tip,
            candidate_sha: c.sha.clone(),
            merged_sha: None,
            mode: if p.branch.status == "already_contained" {
                "already_contained"
            } else if p.branch.checked_out.is_some() {
                "ff-merge"
            } else {
                "update-ref"
            }
            .into(),
            diff_sha256: digest.into(),
            files_changed: p.files_changed,
            insertions: p.insertions,
            deletions: p.deletions,
            intent_at: chrono::Utc::now().to_rfc3339(),
            merged_at: None,
            operator: format!("{} <{}>", name.trim(), email.trim()),
            recovered: false,
        };
        store(home, lib, &fresh, &r)?;
        r
    };
    git(
        &ws.root,
        &["update-ref", &format!("refs/aep/deliveries/{run}"), &c.sha],
    )?;
    let actual = commit(&ws.root, &reference)?;
    if r.recovery(&actual) == Recovery::Record {
        r.recovered = fresh.merge.is_some();
    } else {
        ensure!(
            r.recovery(&actual) == Recovery::Retry,
            "{}",
            Recovery::Changed.message()
        );
        let p = preview(home, &fresh, target)?;
        ensure!(
            p.diff_sha256 == r.diff_sha256,
            "目标或变更已改变，请重新查看 Diff"
        );
        ensure!(p.branch.writable(), "{}", p.branch.reason);
        if r.mode != "already_contained" {
            if let Some(path) = p.branch.checked_out {
                // Recheck symbolic HEAD immediately before touching the checkout.
                ensure!(
                    git(&path, &["symbolic-ref", "-q", "HEAD"])?.trim() == reference,
                    "目标工作区已切换分支"
                );
                git(
                    &path,
                    &[
                        "-c",
                        "merge.autoStash=false",
                        "merge",
                        "--ff-only",
                        "--no-autostash",
                        &c.sha,
                    ],
                )?;
                r.mode = "ff-merge".into();
            } else {
                git(&ws.root, &["update-ref", &reference, &c.sha, &r.before_sha])?;
                r.mode = "update-ref".into();
            }
        }
    }
    let actual = commit(&ws.root, &reference)?;
    ensure!(
        if r.mode == "already_contained" {
            actual == r.before_sha
        } else {
            actual == c.sha
        },
        "合并结果与已验收提交不一致"
    );
    r.status = MergeStatus::Merged;
    r.merged_sha = Some(actual);
    r.merged_at = Some(chrono::Utc::now().to_rfc3339());
    let mut file = tempfile::NamedTempFile::new_in(fresh.directory(home))?;
    serde_json::to_writer_pretty(&mut file, &r)?;
    file.as_file().sync_all()?;
    file.persist_noclobber(fresh.directory(home).join("merge.json"))?;
    store(home, lib, &fresh, &r)?;
    Ok(r)
}
pub fn abandon(
    home: &Path,
    lib: &mut Library,
    project: &str,
    work: &str,
    run: &str,
    reason: &str,
) -> Result<()> {
    ensure!(!reason.trim().is_empty(), "请填写放弃原因");
    lib.change(home, |next| {
        let d = delivery_mut(next, project, work, run)?;
        ensure!(receipt(home, d)?.is_none(), "已完成的合并不能放弃");
        let r = d.merge.as_ref().context("没有待处理合并意图")?;
        ensure!(r.status == MergeStatus::Pending, "已完成的合并不能放弃");
        let ws = Workspace::open(&r.repository)?;
        let lock = fs::OpenOptions::new()
            .write(true)
            .open(ws.data.join("local-delivery.lock"))?;
        lock.try_lock().context("此仓库正在执行本地合并")?;
        let tip = commit(&ws.root, &branch_ref(&r.target_branch, &ws.root)?)?;
        ensure!(tip != r.candidate_sha, "分支已经合并，请恢复补记回执");
        d.merge_history.push(AbandonedIntent {
            intent: r.clone(),
            at: chrono::Utc::now().to_rfc3339(),
            reason: reason.trim().into(),
        });
        d.merge = None;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::save_json;
    use crate::project::{Kind, timeline::Timeline};
    use crate::workflow::Template;
    use serde_json::json;

    struct Fixture {
        _tmp: tempfile::TempDir,
        home: PathBuf,
        repo: PathBuf,
        stage: PathBuf,
        lib: Library,
        d: Delivery,
        base: String,
        candidate: String,
    }
    impl Fixture {
        fn new() -> Result<Self> {
            let tmp = tempfile::tempdir()?;
            let repo = tmp.path().join("repo");
            fs::create_dir(&repo)?;
            git(&repo, &["init", "-b", "main"])?;
            git(&repo, &["config", "user.name", "Merge Test"])?;
            git(&repo, &["config", "user.email", "merge@example.test"])?;
            git(&repo, &["config", "core.autocrlf", "false"])?;
            fs::write(repo.join("file.txt"), "base\n")?;
            git(&repo, &["add", "."])?;
            git(&repo, &["commit", "-m", "base"])?;
            let base = commit(&repo, "HEAD")?;
            git(&repo, &["checkout", "-b", "build"])?;
            fs::write(repo.join("file.txt"), "base\ncandidate\n")?;
            git(&repo, &["add", "."])?;
            git(&repo, &["commit", "-m", "candidate"])?;
            let candidate = commit(&repo, "HEAD")?;
            git(&repo, &["branch", "target", &base])?;
            git(
                &repo,
                &["update-ref", "refs/aep/runs/relay-demo-r2-a1", &candidate],
            )?;
            git(
                &repo,
                &[
                    "remote",
                    "add",
                    "origin",
                    "https://unreachable.invalid/repo.git",
                ],
            )?;
            let stage = tmp.path().join("stage");
            git(
                &repo,
                &[
                    "worktree",
                    "add",
                    "--detach",
                    stage.to_str().unwrap(),
                    &candidate,
                ],
            )?;
            let home = tmp.path().join("home");
            let mut lib = Library::default();
            lib.change(&home, |l| {
                l.create("Demo", vec![repo.clone()])?;
                l.project_mut()?
                    .add_work("Delivery", Kind::Feature, Template::Software)
            })?;
            let p = lib.project().unwrap();
            let w = p.selected().unwrap();
            let d = Delivery {
                project: p.id.clone(),
                work: w.id.clone(),
                run: "deliver".into(),
                accepted_run: "accept".into(),
                archived_at: "2026-11-23T00:00:00Z".into(),
                attempts: 4,
                merge: None,
                merge_history: vec![],
            };
            let transfers: Vec<_> = [(3, "relay-demo-r2-a1", Some(candidate.clone())), (4, "test", None), (5, "accept", None)]
                .into_iter().map(|(from, run, c)| json!({"project":d.project,"work":d.work,"from":from,"to":from+1,"run":run,
                    "repository":repo,"candidate":c,"decision_id":null,"decision":"approved","acceptance":"PASS","confirmed_at":"now"})).collect();
            let runs = ["relay-demo-r2-a1", "test", "accept", "deliver"];
            let rounds: Vec<_> = runs.iter().enumerate().map(|(i, r)| json!({"record":"workflow","run":r,"repository":repo,"stage_index":i+3,
                "round":if i==0 {Some(2)} else {None},"attempt":1,"status":if i==0 {"awaiting_human_review"} else {"succeeded"},"decision":null,
                "read_stage_transfer":if i==0 {serde_json::Value::Null} else {transfers[i-1].clone()},"agents":[]})).collect();
            let history = json!({"deliveries":[d],"work_id":d.work,"work_name":"Delivery","stages":[],"rounds":rounds,"stage_transfers":transfers,"errors":[]});
            let directory = d.directory(&home);
            fs::create_dir_all(&directory)?;
            save_json(&directory, "manifest.json", &json!({"delivery":d}))?;
            save_json(&directory, "timeline.json", &history)?;
            for (i, r) in runs.iter().enumerate() {
                let folder = directory.join(super::super::archive::run_directory(i, r));
                fs::create_dir_all(&folder)?;
                let ev = if i == 0 {
                    json!({"run_id":r,"candidate_sha":candidate,"spec_sha":base,"check_exit_code":0,"target":"target"})
                } else {
                    json!({"run_id":r,"status":"succeeded","workspace":stage,"read_stage_transfer":transfers[i-1]})
                };
                save_json(&folder, "evidence.json", &ev)?;
            }
            lib.change(&home, |l| {
                l.project_mut()?.selected_mut()?.deliveries.push(d.clone());
                Ok(())
            })?;
            Ok(Self {
                _tmp: tmp,
                home,
                repo,
                stage,
                lib,
                d,
                base,
                candidate,
            })
        }
        fn preview(&self, branch: &str) -> Result<Preview> {
            preview(&self.home, &self.d, branch)
        }
        fn merge(&mut self, branch: &str, digest: &str) -> Result<MergeReceipt> {
            merge(
                &self.home,
                &mut self.lib,
                &self.d.project,
                &self.d.work,
                &self.d.run,
                branch,
                digest,
            )
        }
        fn mutate_history(&self, edit: impl FnOnce(&mut serde_json::Value)) -> Result<()> {
            let dir = self.d.directory(&self.home);
            let mut h = read(&dir.join("timeline.json"))?;
            edit(&mut h);
            save_json(&dir, "timeline.json", &h)
        }
        fn pending(&mut self, branch: &str) -> Result<MergeReceipt> {
            let p = self.preview(branch)?;
            let r = MergeReceipt {
                status: MergeStatus::Pending,
                delivery_run: self.d.run.clone(),
                repository: self.repo.clone(),
                target_branch: branch.into(),
                before_sha: p.branch.tip,
                candidate_sha: self.candidate.clone(),
                merged_sha: None,
                mode: if p.branch.checked_out.is_some() {
                    "ff-merge"
                } else {
                    "update-ref"
                }
                .into(),
                diff_sha256: p.diff_sha256,
                files_changed: p.files_changed,
                insertions: p.insertions,
                deletions: p.deletions,
                intent_at: "now".into(),
                merged_at: None,
                operator: "test".into(),
                recovered: false,
            };
            store(&self.home, &mut self.lib, &self.d, &r)?;
            Ok(r)
        }
    }
    #[test]
    fn unchecked_target_merge_is_sealed_read_only_and_idempotent() -> Result<()> {
        let mut f = Fixture::new()?;
        let dir = f.d.directory(&f.home);
        let sealed: Vec<_> = ["manifest.json", "timeline.json"]
            .iter()
            .map(|n| fs::read(dir.join(n)).unwrap())
            .collect();
        let refs = git(&f.repo, &["for-each-ref"])?;
        let catalog = fs::read(f.home.join("projects.json"))?;
        let p = f.preview("target")?;
        assert_eq!(p.candidate.sha, f.candidate);
        assert_eq!(p.files_changed, 1);
        assert_eq!(p.insertions, 1);
        assert_eq!(p.deletions, 0);
        assert!(p.same_as_accepted);
        assert_eq!(refs, git(&f.repo, &["for-each-ref"])?);
        assert_eq!(catalog, fs::read(f.home.join("projects.json"))?);
        let head = commit(&f.repo, "HEAD")?;
        let content = fs::read(f.repo.join("file.txt"))?;
        let r = f.merge("target", &p.diff_sha256)?;
        assert_eq!(r.mode, "update-ref");
        assert_eq!(r.merged_sha.as_ref(), Some(&f.candidate));
        assert_eq!(commit(&f.repo, "HEAD")?, head);
        assert_eq!(fs::read(f.repo.join("file.txt"))?, content);
        for (i, n) in ["manifest.json", "timeline.json"].iter().enumerate() {
            assert_eq!(fs::read(dir.join(n))?, sealed[i]);
        }
        let reopened = Library::open(&f.home)?;
        let d = &reopened.project().unwrap().selected().unwrap().deliveries[0];
        assert_eq!(d.merge.as_ref(), Some(&r));
        assert!(d.snapshot(&f.home).is_ok());
        let receipt_bytes = fs::read(dir.join("merge.json"))?;
        assert_eq!(f.merge("target", "ignored-on-repeat")?, r);
        assert!(f.merge("main", &p.diff_sha256).is_err());
        assert_eq!(fs::read(dir.join("merge.json"))?, receipt_bytes);
        // Old binary lost the catalog mirror: the authoritative new file restores it.
        let mut raw = read(&f.home.join("projects.json"))?;
        raw["projects"][0]["work"][0]["deliveries"][0]
            .as_object_mut()
            .unwrap()
            .remove("merge");
        save_json(&f.home, "projects.json", &raw)?;
        assert_eq!(
            Library::open(&f.home)?
                .project()
                .unwrap()
                .selected()
                .unwrap()
                .deliveries[0]
                .merge
                .as_ref(),
            Some(&r)
        );
        Ok(())
    }
    #[test]
    fn checked_out_main_requires_clean_worktree_and_fast_forwards_files() -> Result<()> {
        let mut f = Fixture::new()?;
        git(&f.repo, &["checkout", "main"])?;
        fs::write(f.repo.join("untracked"), "dirty")?;
        let p = f.preview("main")?;
        assert_eq!(p.branch.status, "unwritable");
        assert!(f.merge("main", &p.diff_sha256).is_err());
        assert_eq!(commit(&f.repo, "main")?, f.base);
        fs::remove_file(f.repo.join("untracked"))?;
        let p = f.preview("main")?;
        let r = f.merge("main", &p.diff_sha256)?;
        assert_eq!(r.mode, "ff-merge");
        assert_eq!(commit(&f.repo, "HEAD")?, f.candidate);
        assert_eq!(
            fs::read_to_string(f.repo.join("file.txt"))?,
            "base\ncandidate\n"
        );
        assert_eq!(commit(&f.stage, "HEAD")?, f.candidate);
        Ok(())
    }
    #[test]
    fn stale_digest_and_divergence_write_no_intent() -> Result<()> {
        let mut f = Fixture::new()?;
        let p = f.preview("target")?;
        git(&f.repo, &["update-ref", "refs/heads/target", &f.candidate])?;
        let catalog = fs::read(f.home.join("projects.json"))?;
        assert!(
            f.merge("target", &p.diff_sha256)
                .unwrap_err()
                .to_string()
                .contains("重新查看 Diff")
        );
        assert_eq!(catalog, fs::read(f.home.join("projects.json"))?);
        git(&f.repo, &["checkout", "main"])?;
        fs::write(f.repo.join("diverged"), "other")?;
        git(&f.repo, &["add", "."])?;
        git(&f.repo, &["commit", "-m", "diverged"])?;
        let p = f.preview("main")?;
        assert_eq!(p.branch.status, "diverged");
        assert_eq!(p.branch.merge_base.as_deref(), Some(f.base.as_str()));
        assert!(f.merge("main", &p.diff_sha256).is_err());
        assert!(!f.d.directory(&f.home).join("merge.json").exists());
        Ok(())
    }
    #[test]
    fn already_contained_records_actual_descendant_without_moving_it() -> Result<()> {
        let mut f = Fixture::new()?;
        fs::write(f.repo.join("later"), "descendant")?;
        git(&f.repo, &["add", "."])?;
        git(&f.repo, &["commit", "-m", "later"])?;
        let tip = commit(&f.repo, "HEAD")?;
        let p = f.preview("build")?;
        assert_eq!(p.branch.status, "already_contained");
        let r = f.merge("build", &p.diff_sha256)?;
        assert_eq!(r.mode, "already_contained");
        assert_eq!(r.merged_sha.as_ref(), Some(&tip));
        assert_eq!(commit(&f.repo, "HEAD")?, tip);
        Ok(())
    }
    #[test]
    fn linked_worktree_is_never_modified() -> Result<()> {
        let mut f = Fixture::new()?;
        let other = f._tmp.path().join("other");
        git(
            &f.repo,
            &["worktree", "add", other.to_str().unwrap(), "target"],
        )?;
        let p = f.preview("target")?;
        assert_eq!(p.branch.status, "unwritable");
        assert!(p.branch.reason.contains(other.to_str().unwrap()));
        assert!(f.merge("target", &p.diff_sha256).is_err());
        assert_eq!(commit(&other, "HEAD")?, f.base);
        Ok(())
    }
    #[test]
    fn missing_chain_ambiguous_candidates_and_changed_run_ref_are_rejected() -> Result<()> {
        let f = Fixture::new()?;
        git(
            &f.repo,
            &["update-ref", "refs/aep/runs/relay-demo-r2-a1", &f.base],
        )?;
        assert!(f.preview("target").is_err());
        git(
            &f.repo,
            &["update-ref", "refs/aep/runs/relay-demo-r2-a1", &f.candidate],
        )?;
        f.mutate_history(|h| {
            h["rounds"][2]["read_stage_transfer"] = serde_json::Value::Null;
        })?;
        assert!(f.preview("target").is_err());
        let g = Fixture::new()?;
        g.mutate_history(|h| {
            let t = h["stage_transfers"][0].clone();
            h["stage_transfers"].as_array_mut().unwrap().push(t);
        })?;
        assert!(g.preview("target").is_err());
        Ok(())
    }
    #[test]
    fn prior_stage_transfers_do_not_override_the_sealed_candidate() -> Result<()> {
        let mut f = Fixture::new()?;
        f.mutate_history(|h| {
            let mut previous = h["stage_transfers"][0].clone();
            previous["run"] = json!("relay-previous-r2-a1");
            previous["candidate"] = json!(f.base);
            h["stage_transfers"].as_array_mut().unwrap().push(previous);
        })?;
        let p = f.preview("target")?;
        assert_eq!(p.candidate.sha, f.candidate);
        assert_eq!(
            f.merge("target", &p.diff_sha256)?.merged_sha,
            Some(f.candidate)
        );
        Ok(())
    }
    #[test]
    fn later_stage_dirty_or_committed_changes_are_not_silently_dropped() -> Result<()> {
        let f = Fixture::new()?;
        fs::write(f.stage.join("untracked"), "not accepted")?;
        let e = f.preview("target").unwrap_err().to_string();
        assert!(e.contains("未纳入"));
        assert!(e.contains("untracked"));
        git(&f.stage, &["add", "."])?;
        git(&f.stage, &["commit", "-m", "unaccepted"])?;
        assert!(
            f.preview("target")
                .unwrap_err()
                .to_string()
                .contains("untracked")
        );
        Ok(())
    }
    #[test]
    fn missing_downstream_workspace_cannot_bypass_validation() -> Result<()> {
        let mut f = Fixture::new()?;
        let p = f.preview("target")?;
        let path =
            f.d.directory(&f.home)
                .join(super::super::archive::run_directory(1, "test"));
        let mut ev = read(&path.join("evidence.json"))?;
        ev.as_object_mut().unwrap().remove("workspace");
        save_json(&path, "evidence.json", &ev)?;
        let catalog = fs::read(f.home.join("projects.json"))?;
        assert!(
            f.preview("target")
                .unwrap_err()
                .to_string()
                .contains("缺少工作区")
        );
        assert!(f.merge("target", &p.diff_sha256).is_err());
        assert_eq!(commit(&f.repo, "target")?, f.base);
        assert_eq!(fs::read(f.home.join("projects.json"))?, catalog);
        Ok(())
    }
    #[test]
    fn pending_recovers_tip_equal_candidate_or_retries_original_tip() -> Result<()> {
        for moved in [false, true] {
            let mut f = Fixture::new()?;
            let r = f.pending("target")?;
            assert_eq!(r.recovery(&f.base), Recovery::Retry);
            assert_eq!(r.recovery(&f.candidate), Recovery::Record);
            if moved {
                git(&f.repo, &["update-ref", "refs/heads/target", &f.candidate])?;
            }
            let done = f.merge("target", &r.diff_sha256)?;
            assert_eq!(done.recovered, moved);
            assert_eq!(done.merged_sha.as_ref(), Some(&f.candidate));
        }
        Ok(())
    }
    #[test]
    fn pending_other_tip_blocks_until_explicit_abandon_preserves_history() -> Result<()> {
        let mut f = Fixture::new()?;
        let r = f.pending("target")?;
        fs::write(f.repo.join("later"), "later")?;
        git(&f.repo, &["add", "."])?;
        git(&f.repo, &["commit", "-m", "later"])?;
        let tip = commit(&f.repo, "HEAD")?;
        assert_eq!(r.recovery(&tip), Recovery::Changed);
        git(&f.repo, &["update-ref", "refs/heads/target", &tip])?;
        assert!(
            f.merge("target", &r.diff_sha256)
                .unwrap_err()
                .to_string()
                .contains("合并过程中被修改")
        );
        abandon(
            &f.home,
            &mut f.lib,
            &f.d.project,
            &f.d.work,
            &f.d.run,
            "target changed",
        )?;
        let d = &f.lib.project().unwrap().selected().unwrap().deliveries[0];
        assert!(d.merge.is_none());
        assert_eq!(d.merge_history.len(), 1);
        assert_eq!(d.merge_history[0].intent, r);
        assert!(d.snapshot(&f.home).is_ok());
        Ok(())
    }
    #[test]
    fn shared_local_ticket_lock_prevents_merge_and_no_intent_is_created() -> Result<()> {
        let mut f = Fixture::new()?;
        let p = f.preview("target")?;
        let ws = Workspace::open(&f.repo)?;
        fs::create_dir_all(&ws.data)?;
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(ws.data.join("local-delivery.lock"))?;
        lock.lock()?;
        assert!(f.merge("target", &p.diff_sha256).is_err());
        assert!(
            Library::open(&f.home)?
                .project()
                .unwrap()
                .selected()
                .unwrap()
                .deliveries[0]
                .merge
                .is_none()
        );
        assert_eq!(commit(&f.repo, "target")?, f.base);
        Ok(())
    }
    #[test]
    fn forced_duplicate_checkout_is_also_unwritable() -> Result<()> {
        let f = Fixture::new()?;
        let linked = f._tmp.path().join("duplicate");
        git(
            &f.repo,
            &[
                "worktree",
                "add",
                "--force",
                linked.to_str().unwrap(),
                "build",
            ],
        )?;
        let p = f.preview("build")?;
        assert_eq!(p.branch.status, "unwritable");
        Ok(())
    }
    #[test]
    fn changed_retained_ref_after_preview_rejects_confirmation() -> Result<()> {
        let mut f = Fixture::new()?;
        let p = f.preview("target")?;
        git(
            &f.repo,
            &["update-ref", "refs/aep/runs/relay-demo-r2-a1", &f.base],
        )?;
        let catalog = fs::read(f.home.join("projects.json"))?;
        assert!(f.merge("target", &p.diff_sha256).is_err());
        assert_eq!(commit(&f.repo, "target")?, f.base);
        assert_eq!(fs::read(f.home.join("projects.json"))?, catalog);
        Ok(())
    }
    #[test]
    fn unrelated_branch_does_not_hide_valid_targets() -> Result<()> {
        let f = Fixture::new()?;
        let tree = git(&f.repo, &["rev-parse", "HEAD^{tree}"])?;
        let orphan = git(&f.repo, &["commit-tree", tree.trim(), "-m", "unrelated"])?;
        git(&f.repo, &["branch", "orphan", orphan.trim()])?;
        let c = candidate(&f.home, &f.d)?;
        let bs = branches(&c)?;
        assert_eq!(
            bs.iter().find(|b| b.name == "orphan").unwrap().status,
            "diverged"
        );
        assert!(bs.iter().find(|b| b.name == "target").unwrap().writable());
        Ok(())
    }
    #[test]
    fn legacy_delivery_deserializes_and_snapshot_validates() -> Result<()> {
        let f = Fixture::new()?;
        let bytes = fs::read(f.d.directory(&f.home).join("manifest.json"))?;
        assert!(!String::from_utf8_lossy(&bytes).contains("merge"));
        let d: Delivery = serde_json::from_value(
            read(&f.d.directory(&f.home).join("manifest.json"))?["delivery"].clone(),
        )?;
        assert!(d.merge.is_none());
        let _: Timeline = d.snapshot(&f.home)?;
        assert!(f.preview("target").is_ok());
        Ok(())
    }
    #[test]
    fn large_binary_diff_exports_full_patch_without_preview_writes() -> Result<()> {
        let mut f = Fixture::new()?;
        let text = (0..20000)
            .map(|i| format!("line {i}\n"))
            .collect::<String>();
        fs::write(f.repo.join("large"), &text)?;
        fs::write(f.repo.join("binary"), [0, 1, 2, 3])?;
        git(&f.repo, &["add", "."])?;
        git(&f.repo, &["commit", "-m", "large"])?;
        // Use a fresh approved fixture candidate to exercise preview/patch size handling.
        let next = commit(&f.repo, "HEAD")?;
        git(
            &f.repo,
            &["update-ref", "refs/aep/runs/relay-demo-r2-a1", &next],
        )?;
        git(&f.stage, &["reset", "--hard", &next])?;
        f.mutate_history(|h| {
            h["stage_transfers"][0]["candidate"] = json!(next);
            h["rounds"][1]["read_stage_transfer"]["candidate"] = json!(next);
        })?;
        let source =
            f.d.directory(&f.home)
                .join(super::super::archive::run_directory(0, "relay-demo-r2-a1"));
        let mut ev = read(&source.join("evidence.json"))?;
        ev["candidate_sha"] = json!(next);
        save_json(&source, "evidence.json", &ev)?;
        let folder =
            f.d.directory(&f.home)
                .join(super::super::archive::run_directory(1, "test"));
        let mut ev = read(&folder.join("evidence.json"))?;
        ev["read_stage_transfer"]["candidate"] = json!(next);
        save_json(&folder, "evidence.json", &ev)?;
        f.candidate = next;
        let p = f.preview("target")?;
        assert!(p.truncated);
        assert!(p.diff.len() <= DIFF_LIMIT);
        assert_eq!(p.files_changed, 3);
        let path = f._tmp.path().join("full.patch");
        export_patch(&f.home, &f.d, "target", &path)?;
        let bytes = fs::read(&path)?;
        assert!(bytes.len() > DIFF_LIMIT);
        assert!(String::from_utf8_lossy(&bytes).contains("GIT binary patch"));
        assert!(export_patch(&f.home, &f.d, "target", &path).is_err());
        Ok(())
    }
}
