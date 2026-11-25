//! A repository's remote side (specs/remote-linkage): which remote branch items start from and
//! follow, and how a delivery reaches it, by pushing a branch and opening or updating a PR.
//! Pushing happens only from an explicit person-confirmed `submit`; nothing here runs on its own.
use super::{Library, archive::Delivery, merge};
use crate::workspace::{git, ident};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryMode {
    /// Fast-forward a local branch (the original behaviour).
    #[default]
    Local,
    /// Push the accepted commit and open or update a pull request.
    PullRequest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Forge {
    Gitea,
    GitHub,
    GitLab,
}

/// Per repository of a project (`Project.upstreams`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Upstream {
    pub remote: String,
    pub target: String,
    #[serde(default)]
    pub delivery: DeliveryMode,
    #[serde(default = "prefix")]
    pub branch_prefix: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forge: Option<Forge>,
    /// Web address of the repository (`https://host/owner/repo`) when it cannot be derived
    /// from the remote URL (SSH host aliases).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web: Option<String>,
}
fn prefix() -> String {
    "aep/".into()
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.contains("..")
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || "/._-".contains(c))
}

impl Upstream {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            valid_name(&self.remote) && !self.remote.contains('/'),
            "远端名无效"
        );
        ensure!(valid_name(&self.target), "目标分支名无效");
        ensure!(
            self.branch_prefix.is_empty() || valid_name(&self.branch_prefix),
            "分支前缀无效"
        );
        if let Some(web) = &self.web {
            ensure!(
                web.starts_with("https://") || web.starts_with("http://"),
                "网页地址需以 http(s):// 开头"
            );
        }
        Ok(())
    }
    /// The remote-tracking ref items start from and follow.
    pub fn tracking(&self) -> String {
        format!("refs/remotes/{}/{}", self.remote, self.target)
    }
    pub fn label(&self) -> String {
        format!("{}/{}", self.remote, self.target)
    }
}

/// Defaults for a repository: the checked-out branch when it tracks a remote branch (dev →
/// gitea/dev), else the remote's HEAD branch; the forge and web address from the remote URL.
pub fn detect(repo: &Path) -> Option<Upstream> {
    let remotes: Vec<String> = git(repo, &["remote"])
        .ok()?
        .lines()
        .map(str::to_owned)
        .collect();
    let tracked = git(
        repo,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )
    .ok()
    .map(|s| s.trim().to_owned())
    .and_then(|s| {
        let (remote, branch) = s.split_once('/')?;
        remotes
            .iter()
            .any(|r| r == remote)
            .then(|| (remote.to_owned(), branch.to_owned()))
    });
    let (remote, target) = match tracked {
        Some(t) => t,
        None => {
            let remote = remotes
                .iter()
                .find(|r| *r == "origin")
                .or(remotes.first())?
                .clone();
            let head = git(
                repo,
                &[
                    "symbolic-ref",
                    "--short",
                    &format!("refs/remotes/{remote}/HEAD"),
                ],
            )
            .ok()
            .and_then(|s| s.trim().split_once('/').map(|(_, b)| b.to_owned()))
            .unwrap_or_else(|| "main".into());
            (remote, head)
        }
    };
    let url = git(repo, &["remote", "get-url", &remote]).unwrap_or_default();
    let (forge, web) = from_url(url.trim());
    Some(Upstream {
        remote,
        target,
        delivery: DeliveryMode::Local,
        branch_prefix: prefix(),
        forge,
        web,
    })
}

/// Forge and web address from a remote URL; SSH host aliases give neither.
pub fn from_url(url: &str) -> (Option<Forge>, Option<String>) {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .map(|r| r.split_once('@').map_or(r, |(_, h)| h).to_owned())
        .or_else(|| {
            let r = url.strip_prefix("ssh://").unwrap_or(url);
            let r = r.split_once('@').map_or(r, |(_, h)| h);
            // scp form host:owner/repo, or ssh://host[:port]/owner/repo
            if url.starts_with("ssh://") {
                let (host, path) = r.split_once('/')?;
                Some(format!("{}/{path}", host.split(':').next()?))
            } else {
                let (host, path) = r.split_once(':')?;
                Some(format!("{host}/{path}"))
            }
        });
    let Some(rest) = rest else {
        return (None, None);
    };
    let rest = rest.trim_end_matches('/').trim_end_matches(".git");
    let host = rest.split('/').next().unwrap_or("");
    let forge = if host == "github.com" {
        Some(Forge::GitHub)
    } else if host.contains("gitlab") {
        Some(Forge::GitLab)
    } else if host.contains("gitea") || host.contains("forgejo") || host.contains("codeberg") {
        Some(Forge::Gitea)
    } else {
        None
    };
    // A host without a dot is an SSH alias (Host entry), not an address a browser can open.
    let web =
        (host.contains('.') && rest.matches('/').count() >= 2).then(|| format!("https://{rest}"));
    (forge, web)
}

pub fn fetch(repo: &Path, up: &Upstream) -> Result<()> {
    up.validate()?;
    git(
        repo,
        &[
            "fetch",
            "-q",
            "--no-tags",
            &up.remote,
            &format!("+refs/heads/{0}:refs/remotes/{1}/{0}", up.target, up.remote),
        ],
    )
    .with_context(|| format!("无法获取 {}", up.label()))?;
    Ok(())
}

pub fn base_sha(repo: &Path, up: &Upstream) -> Result<String> {
    Ok(git(
        repo,
        &[
            "rev-parse",
            "--verify",
            &format!("{}^{{commit}}", up.tracking()),
        ],
    )
    .with_context(|| format!("本地还没有 {}，请先获取远端", up.label()))?
    .trim()
    .to_owned())
}

/// What a delivery's push did (stored on the delivery).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequest {
    pub remote: String,
    pub target: String,
    pub branch: String,
    pub pushed_sha: String,
    /// "update" (an existing PR's branch), "agit", "gitlab", "gh" or "manual".
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<u64>,
    pub title: String,
    pub at: String,
    pub operator: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Preview {
    pub repository: PathBuf,
    pub upstream: Upstream,
    pub candidate: String,
    pub base_sha: String,
    /// The accepted commit contains the target branch: a PR can be merged without conflicts
    /// from the item side.
    pub contains_base: bool,
    pub behind: u64,
    pub conflicts: Vec<String>,
    pub branch: String,
    /// The branch's current commit on the remote (for an update's lease), if it exists.
    pub remote_sha: Option<String>,
    pub title: String,
    pub body: String,
    pub commits: Vec<String>,
    pub previous: Option<PullRequest>,
}

fn slug(name: &str, work: &str) -> String {
    let ascii: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let ascii = ascii
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let short = &work[..work.len().min(8)];
    if ascii.len() >= 3 {
        format!(
            "{}-{short}",
            ascii[..ascii.len().min(40)].trim_end_matches('-')
        )
    } else {
        format!("item-{short}")
    }
}

fn remote_head(repo: &Path, remote: &str, branch: &str) -> Result<Option<String>> {
    let out = git(
        repo,
        &[
            "ls-remote",
            "--heads",
            remote,
            &format!("refs/heads/{branch}"),
        ],
    )?;
    Ok(out.split_whitespace().next().map(str::to_owned))
}

/// Read-only: fetch the target, check the accepted commit against it, propose branch,
/// title and description. `branch` overrides the proposed (or previously pushed) branch.
pub fn preview(home: &Path, lib: &Library, d: &Delivery, branch: Option<&str>) -> Result<Preview> {
    let p = lib
        .projects
        .iter()
        .find(|p| p.id == d.project)
        .context("项目不存在")?;
    let w = p
        .work
        .iter()
        .find(|w| w.id == d.work)
        .context("事项不存在")?;
    let c = merge::candidate(home, d)?;
    let up = p
        .upstreams
        .get(&c.repository)
        .cloned()
        .context("这个仓库还没有配置远端，请先在仓库页设置远端和目标分支")?;
    fetch(&c.repository, &up)?;
    let base = base_sha(&c.repository, &up)?;
    let contains_base = git(
        &c.repository,
        &["merge-base", "--is-ancestor", &base, &c.sha],
    )
    .is_ok();
    let behind: u64 = git(
        &c.repository,
        &["rev-list", "--count", &format!("{}..{base}", c.sha)],
    )?
    .trim()
    .parse()
    .unwrap_or(0);
    let conflicts = if contains_base {
        vec![]
    } else {
        conflict_files(&c.repository, &c.sha, &base)?
    };
    let previous = d.pull_request.clone();
    let branch = branch
        .map(str::to_owned)
        .or_else(|| previous.as_ref().map(|p| p.branch.clone()))
        .unwrap_or_else(|| format!("{}{}", up.branch_prefix, slug(&w.name, &w.id)));
    ensure!(valid_name(&branch), "分支名无效：{branch}");
    let remote_sha = remote_head(&c.repository, &up.remote, &branch)?;
    let commits = git(
        &c.repository,
        &[
            "log",
            "--format=%h %s",
            "--no-merges",
            "-n",
            "60",
            &format!("{base}..{}", c.sha),
        ],
    )?
    .lines()
    .map(str::to_owned)
    .collect();
    let handoff = std::fs::read_to_string(
        crate::workflow::attempt_dir(home, &d.project, &d.work, &d.run).join("result.txt"),
    )
    .ok()
    .and_then(|r| {
        r.lines()
            .rev()
            .find(|l| l.starts_with("AEP-HANDOFF:"))
            .map(|l| l.trim_start_matches("AEP-HANDOFF:").trim().to_owned())
    });
    let mut body = format!("由 AEP Studio 事项「{}」交付。\n", w.name);
    if let Some(h) = handoff {
        body.push_str(&format!("\n## 交付结论\n{h}\n"));
    }
    body.push_str(&format!("\n验收提交：{}\n", &c.sha[..c.sha.len().min(12)]));
    Ok(Preview {
        repository: c.repository,
        title: w.name.clone(),
        body,
        upstream: up,
        candidate: c.sha,
        base_sha: base,
        contains_base,
        behind,
        conflicts,
        branch,
        remote_sha,
        commits,
        previous,
    })
}

fn conflict_files(repo: &Path, a: &str, b: &str) -> Result<Vec<String>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            a,
            b,
        ])
        .output()?;
    Ok(match out.status.code() {
        Some(1) => String::from_utf8_lossy(&out.stdout)
            .lines()
            .skip(1)
            .take_while(|l| !l.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => vec![],
    })
}

pub struct Submit<'a> {
    pub branch: &'a str,
    pub title: &'a str,
    pub body: &'a str,
    /// The remote branch commit the preview saw (None: the branch must not exist yet). The
    /// push uses it as `--force-with-lease`, so a branch someone moved since is never lost.
    pub expect: Option<&'a str>,
    /// Open a PR for a new branch; false only pushes (an existing PR picks it up).
    pub open: bool,
    pub number: Option<u64>,
}

/// Push the accepted commit and open or update its PR; recorded on the delivery.
pub fn submit(
    home: &Path,
    lib: &mut Library,
    project: &str,
    work: &str,
    s: Submit,
) -> Result<PullRequest> {
    ensure!(valid_name(s.branch), "分支名无效：{}", s.branch);
    ensure!(
        !s.title.trim().is_empty() && !s.title.contains('\n'),
        "PR 标题需为一行"
    );
    ident(project, 64)?;
    ident(work, 64)?;
    let d = lib
        .projects
        .iter()
        .find(|p| p.id == project)
        .and_then(|p| p.work.iter().find(|w| w.id == work))
        .and_then(|w| w.deliveries.last())
        .cloned()
        .context("事项还没有交付记录")?;
    let pv = preview(home, lib, &d, Some(s.branch))?;
    ensure!(
        pv.conflicts.is_empty(),
        "验收的提交和 {} 有冲突（落后 {} 个提交，冲突：{}）。请退回开发合入 {} 并解决冲突，重新验收后再提交",
        pv.upstream.label(),
        pv.behind,
        pv.conflicts.join("、"),
        pv.upstream.label()
    );
    ensure!(
        pv.remote_sha.as_deref() == s.expect,
        "远端分支 {} 在预览之后变了（现在是 {}），请重新预览核对",
        s.branch,
        pv.remote_sha.as_deref().unwrap_or("不存在")
    );
    let up = &pv.upstream;
    let repo = &pv.repository;
    let lease = format!(
        "--force-with-lease=refs/heads/{}:{}",
        s.branch,
        s.expect.unwrap_or("")
    );
    let refspec = format!("{}:refs/heads/{}", pv.candidate, s.branch);
    let mut url = None;
    let mut number = s.number;
    let mode = if s.expect.is_some() || !s.open {
        push(repo, &[&lease, &up.remote, &refspec])?;
        "update"
    } else {
        match up.forge {
            Some(Forge::GitLab) => {
                let target = format!("merge_request.target={}", up.target);
                let title = format!("merge_request.title={}", s.title.trim());
                let desc = format!("merge_request.description={}", one_line(s.body));
                let out = push(
                    repo,
                    &[
                        &lease,
                        "-o",
                        "merge_request.create",
                        "-o",
                        &target,
                        "-o",
                        &title,
                        "-o",
                        &desc,
                        &up.remote,
                        &refspec,
                    ],
                )?;
                url = find_url(&out, "merge_requests/");
                "gitlab"
            }
            Some(Forge::Gitea) => {
                // AGit: the PR is created from a push to refs/for/<target>/<topic>; the
                // regular branch is pushed too so the change is visible as a branch.
                push(repo, &[&lease, &up.remote, &refspec])?;
                let topic = s.branch.replace('/', "-");
                let title = format!("title={}", s.title.trim());
                let desc = format!("description={}", one_line(s.body));
                let agit = format!("{}:refs/for/{}/{topic}", pv.candidate, up.target);
                match push(repo, &["-o", &title, "-o", &desc, &up.remote, &agit]) {
                    Ok(out) => {
                        url = find_url(&out, "/pulls/");
                        "agit"
                    }
                    Err(_) => "manual",
                }
            }
            Some(Forge::GitHub) => {
                push(repo, &[&lease, &up.remote, &refspec])?;
                match gh_create(repo, up, s.branch, s.title, s.body) {
                    Ok(u) => {
                        url = Some(u);
                        "gh"
                    }
                    Err(_) => "manual",
                }
            }
            None => {
                push(repo, &[&lease, &up.remote, &refspec])?;
                "manual"
            }
        }
    };
    if url.is_none() && mode == "manual" {
        url = compare_url(up, s.branch);
    }
    if number.is_none() {
        number = url
            .as_deref()
            .and_then(|u| u.rsplit('/').next()?.parse().ok());
    }
    let pr = PullRequest {
        remote: up.remote.clone(),
        target: up.target.clone(),
        branch: s.branch.to_owned(),
        pushed_sha: pv.candidate.clone(),
        mode: mode.into(),
        url,
        number,
        title: s.title.trim().to_owned(),
        at: chrono::Utc::now().to_rfc3339(),
        operator: merge_operator(repo),
    };
    let (run, stored) = (d.run.clone(), pr.clone());
    lib.change(home, move |next| {
        let d = next
            .projects
            .iter_mut()
            .find(|p| p.id == project)
            .and_then(|p| p.work.iter_mut().find(|w| w.id == work))
            .and_then(|w| w.deliveries.iter_mut().find(|d| d.run == run))
            .context("归档交付不存在")?;
        d.pull_request = Some(stored);
        Ok(())
    })?;
    Ok(pr)
}

fn merge_operator(repo: &Path) -> String {
    git(repo, &["config", "user.name"])
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

/// Push options cannot carry newlines.
fn one_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

fn push(repo: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("push")
        .args(args)
        .env_remove(crate::CONTROL_OPERATOR_TOKEN)
        .env_remove(crate::CONTROL_DEVICE_TOKEN)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .context("start git push")?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if !out.status.success() {
        bail!("推送失败：{}", text.trim());
    }
    Ok(text)
}

fn find_url(out: &str, marker: &str) -> Option<String> {
    out.split_whitespace()
        .find(|w| (w.starts_with("https://") || w.starts_with("http://")) && w.contains(marker))
        .map(|w| w.trim_end_matches(['.', ',']).to_owned())
}

fn compare_url(up: &Upstream, branch: &str) -> Option<String> {
    let web = up.web.as_deref()?.trim_end_matches('/');
    Some(match up.forge {
        Some(Forge::GitLab) => format!(
            "{web}/-/merge_requests/new?merge_request[source_branch]={branch}&merge_request[target_branch]={}",
            up.target
        ),
        Some(Forge::GitHub) => format!("{web}/compare/{}...{branch}?expand=1", up.target),
        _ => format!("{web}/compare/{}...{branch}", up.target),
    })
}

fn gh_create(repo: &Path, up: &Upstream, branch: &str, title: &str, body: &str) -> Result<String> {
    use std::io::Write;
    let web = up.web.as_deref().context("没有仓库网页地址")?;
    let mut child = Command::new("gh")
        .current_dir(repo)
        .args([
            "pr",
            "create",
            "--repo",
            web,
            "--base",
            &up.target,
            "--head",
            branch,
            "--title",
            title,
            "--body-file",
            "-",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .context("gh stdin")?
        .write_all(body.as_bytes())?;
    let out = child.wait_with_output()?;
    ensure!(
        out.status.success(),
        "gh: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    find_url(&String::from_utf8_lossy(&out.stdout), "/pull/").context("gh 没有返回 PR 地址")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_give_forge_and_web_address() {
        assert_eq!(
            from_url("git@github.com:PokeOriginal/PokeOriginal.git"),
            (
                Some(Forge::GitHub),
                Some("https://github.com/PokeOriginal/PokeOriginal".into())
            )
        );
        assert_eq!(
            from_url("https://gitlab.example.com/a/b/c.git"),
            (
                Some(Forge::GitLab),
                Some("https://gitlab.example.com/a/b/c".into())
            )
        );
        assert_eq!(
            from_url("ssh://git@gitea.example.com:2222/Team/Repo.git"),
            (
                Some(Forge::Gitea),
                Some("https://gitea.example.com/Team/Repo".into())
            )
        );
        // An SSH host alias: no forge, no web address.
        assert_eq!(
            from_url("ssh://gitea-poke/Fujisan/PokeOriginal.git"),
            (Some(Forge::Gitea), None)
        );
        assert_eq!(from_url("git@myhost:x/y.git"), (None, None));
    }

    #[test]
    fn names_are_checked_and_slugs_are_ascii() {
        assert!(valid_name("aep/export-csv-12345678"));
        assert!(!valid_name("-x") && !valid_name("a..b") && !valid_name("a b"));
        assert_eq!(slug("导出 CSV 报表", "0123456789"), "csv-01234567");
        assert_eq!(slug("代码结构", "abcdef0123"), "item-abcdef01");
        assert_eq!(one_line("a\n\n b \nc"), "a · b · c");
    }
}
