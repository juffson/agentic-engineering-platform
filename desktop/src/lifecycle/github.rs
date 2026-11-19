use aep::workspace::{Workspace, commit, git, validate_task_target};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

use super::{field, save};
use crate::{Runner, publish, relay};

#[derive(Clone, Serialize, Deserialize)]
pub struct Fixture {
    pub source: PathBuf,
    #[serde(default)]
    pub studio: bool,
    #[serde(default)]
    pub real_task: bool,
    pub repo: String,
    pub task: String,
    pub base: String,
    pub base_sha: String,
    pub spec: String,
    pub actor: String,
}

#[derive(Clone)]
pub(crate) struct Branch {
    pub name: String,
    pub sha: String,
}

pub(crate) struct Choices {
    pub tasks: Vec<relay::Task>,
    pub branches: Vec<Branch>,
    pub task: Option<relay::Task>,
    pub branch: Option<Branch>,
}

pub(crate) fn choices(ws: &Workspace, runner: &Runner) -> Result<Choices> {
    publish::repository(ws, runner)?;
    let output = String::from_utf8(command(
        "git",
        &ws.root,
        &["ls-remote", "--refs", "--heads", "origin"],
    )?)?;
    let tasks = relay::tasks_from_heads(&output);
    let branches = output
        .lines()
        .filter_map(|line| {
            let (sha, reference) = line.split_once('\t')?;
            let name = reference.strip_prefix("refs/heads/")?;
            (!name.starts_with("aep-task/") && !name.starts_with("aep/pr/")).then(|| Branch {
                name: name.to_owned(),
                sha: sha.to_owned(),
            })
        })
        .collect();
    Ok(Choices {
        tasks,
        branches,
        task: None,
        branch: None,
    })
}

pub(super) fn command(program: &str, cwd: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new(program)
        .current_dir(cwd)
        .env("PATH", Runner::default().path()?)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(args)
        .output()
        .with_context(|| format!("start {program}"))?;
    ensure!(
        output.status.success(),
        "{program} {}: {}",
        args[0],
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output.stdout)
}

fn gh(cwd: &Path, args: &[&str]) -> Result<Value> {
    Ok(serde_json::from_slice(&command("gh", cwd, args)?)?)
}

impl Fixture {
    pub fn prepare(
        out: &Path,
        root: &Path,
        source: &Path,
        runner: &Runner,
        studio: bool,
    ) -> Result<(Self, Workspace)> {
        Self::prepare_session(out, root, source, runner, studio, None)
    }

    pub(crate) fn prepare_task(
        out: &Path,
        root: &Path,
        source: &Path,
        runner: &Runner,
        task: relay::Task,
        branch: Branch,
    ) -> Result<(Self, Workspace)> {
        Self::prepare_session(out, root, source, runner, true, Some((task, branch)))
    }

    fn prepare_session(
        out: &Path,
        root: &Path,
        source: &Path,
        runner: &Runner,
        studio: bool,
        selected: Option<(relay::Task, Branch)>,
    ) -> Result<(Self, Workspace)> {
        let source_ws = Workspace::open(source)?;
        if selected.is_some() {
            publish::repository(&source_ws, runner)?;
        }
        let source = source_ws.root;
        let binary = runner.binary()?;
        let origin = git(&source, &["remote", "get-url", "origin"])?;
        let origin = origin.trim();
        ensure!(
            [
                "git@github.com:",
                "https://github.com/",
                "ssh://git@github.com/"
            ]
            .iter()
            .any(|prefix| origin.starts_with(prefix)),
            "origin must be on github.com"
        );
        let remote = gh(
            &source,
            &[
                "repo",
                "view",
                origin,
                "--json",
                "nameWithOwner,defaultBranchRef,viewerPermission",
            ],
        )?;
        let repo = field(&remote, "nameWithOwner")?.to_owned();
        let default_branch = field(&remote["defaultBranchRef"], "name")?;
        let clone_branch = selected
            .as_ref()
            .map_or(default_branch, |(_, branch)| &branch.name);
        ensure!(
            matches!(
                remote["viewerPermission"].as_str(),
                Some("ADMIN" | "MAINTAIN" | "WRITE")
            ),
            "the active GitHub account needs write permission on {repo}"
        );
        let user = gh(&source, &["api", "--hostname", "github.com", "user"])?;
        let actor = field(&user, "login")?.to_owned();
        for (key, cli) in [("Codex", "codex"), ("Claude Code", "claude")] {
            command(cli, &source, &["--version"])
                .with_context(|| format!("install {key} before running the live workflow"))?;
        }
        let checkout = root.join("workspace/repo");
        fs::create_dir_all(checkout.parent().context("workspace directory")?)?;
        command(
            "git",
            root,
            &[
                "clone",
                "--no-local",
                "--no-tags",
                "--single-branch",
                "--branch",
                clone_branch,
                "--",
                origin,
                checkout.to_str().context("checkout path")?,
            ],
        )?;
        let ws = Workspace::open(&checkout)?;
        let base_sha = commit(&ws.root, "HEAD")?;
        if selected.is_none() {
            ensure!(
                ws.root.join(".github/workflows/pilot.yml").is_file(),
                "the GitHub default branch must contain the AEP pilot workflow"
            );
        }

        for key in ["user.name", "user.email"] {
            let value = git(&source, &["config", "--get", key])
                .with_context(|| format!("configure Git {key} for the fixture commit"))?;
            git(&ws.root, &["config", "--local", key, value.trim_end()])?;
        }
        if let Some((task, branch)) = selected {
            ensure!(
                base_sha == branch.sha,
                "目标分支已更新，请刷新任务和分支后重选"
            );
            let reference = format!("refs/heads/aep-task/{}", task.name);
            let pinned = "refs/aep/lifecycle/spec";
            git(
                &ws.root,
                &[
                    "fetch",
                    "--no-tags",
                    "origin",
                    &format!("{reference}:{pinned}"),
                ],
            )?;
            let spec = commit(&ws.root, pinned)?;
            ensure!(spec == task.sha, "任务派发版本已变化，请刷新后重选");
            validate_task_target(&ws.root, &base_sha, &spec, &task.name)?;
            let fixture = Self {
                source,
                studio,
                real_task: true,
                repo,
                task: task.name,
                base: branch.name,
                base_sha,
                spec,
                actor,
            };
            for file in ["spec.md", "task.json"] {
                let location = format!("{}:specs/{}/{file}", fixture.spec, fixture.task);
                fs::write(out.join(file), git(&ws.root, &["show", &location])?)?;
            }
            save(out, "fixture.json", &fixture)?;
            return Ok((fixture, ws));
        }
        git(&ws.root, &["switch", "--detach"])?;
        let task = format!("studio-lifecycle-{}", uuid::Uuid::new_v4().simple());
        let base = format!("test/{task}");
        ensure!(
            base != default_branch,
            "test target must differ from the GitHub default"
        );
        let dir = ws.root.join("specs").join(&task);
        fs::create_dir(&dir)?;
        let spec_body = include_str!("spec.md");
        let body = spec_body.replace("{task}", &task);
        let acceptance = format!(
            "set -e; binary=$(mktemp); trap 'rm -- \"$binary\"' EXIT; rustc --edition=2024 --test specs/{task}/acceptance.rs -o \"$binary\"; \"$binary\""
        );
        let manifest = json!({
            "title": "Studio live lifecycle test", "allowed_paths": [format!("specs/{task}/normalize.rs")],
            "check": ["sh", "-c", acceptance]
        });
        fs::write(dir.join("spec.md"), &body)?;
        save(&dir, "task.json", &manifest)?;
        fs::write(
            dir.join("normalize.rs"),
            include_str!("../../examples/support/handoff/normalize.rs"),
        )?;
        fs::write(
            dir.join("acceptance.rs"),
            include_str!("../../examples/support/handoff/acceptance.rs"),
        )?;
        git(&ws.root, &["add", "--", &format!("specs/{task}")])?;
        git(
            &ws.root,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "Pin synthetic Studio lifecycle task",
            ],
        )?;
        let spec = commit(&ws.root, "HEAD")?;
        let fixture = Self {
            source,
            studio,
            real_task: false,
            repo,
            task,
            base,
            base_sha,
            spec,
            actor,
        };
        save(out, "fixture.json", &fixture)?;
        fs::write(out.join("spec.md"), body)?;
        save(out, "task.json", &manifest)?;

        git(
            &ws.root,
            &["switch", "-c", &fixture.base, &fixture.base_sha],
        )?;
        let base_ref = format!("refs/heads/{}", fixture.base);
        git(
            &ws.root,
            &[
                "push",
                "--no-follow-tags",
                "--recurse-submodules=no",
                &format!("--force-with-lease={base_ref}:"),
                "origin",
                &format!("HEAD:{base_ref}"),
            ],
        )?;
        command(
            binary.to_str().context("runner path")?,
            &ws.root,
            &["dispatch", "--task", &fixture.task, "--rev", &fixture.spec],
        )?;
        Ok((fixture, ws))
    }

    pub(crate) fn verify_ci(
        &self,
        ws: &Workspace,
        out: &Path,
        number: u64,
        head: &str,
        run: &str,
    ) -> Result<()> {
        let pr = self.pr(ws, number, head, run)?;
        save(out, "github-ci.json", &pr)?;
        ensure!(
            pr["state"] == "OPEN" && pr["baseRefOid"] == self.base_sha,
            "隔离 PR 已关闭或目标提交已变化"
        );
        if self.real_task && !ws.root.join(".github/workflows/pilot.yml").is_file() {
            ready_checks(&pr)?.context("等待当前 PR 的 GitHub 检查通过")?;
            return Ok(());
        }
        let acceptance = ci_ready(&pr)?.context("等待当前 PR 的 GitHub 独立验收和其他检查完成")?;
        self.checked_workflow(ws, out, head, acceptance)
    }

    pub fn head_branch(&self, run: &str) -> String {
        format!("aep/pr/{}/{run}", self.task)
    }

    pub fn check_publication(
        &self,
        publication: &Value,
        second: &str,
        candidate: &str,
    ) -> Result<u64> {
        ensure!(
            publication["target"]["repo"] == self.repo.to_ascii_lowercase()
                && publication["target"]["base"] == self.base
                && publication["target"]["base_sha"] == self.base_sha
                && publication["target"]["head"] == self.head_branch(second)
                && publication["target"]["spec"] == self.spec
                && publication["target"]["candidate"] == candidate
                && publication["pr"]["base"] == self.base
                && publication["pr"]["head_sha"] == candidate
                && publication["confirmed_by"] == self.actor
                && publication["error"].is_null(),
            "Studio publication does not match the isolated candidate"
        );
        publication["pr"]["number"]
            .as_u64()
            .context("missing PR number")
    }

    pub fn pr(&self, ws: &Workspace, number: u64, head: &str, run: &str) -> Result<Value> {
        let view = gh(
            &ws.root,
            &[
                "pr",
                "view",
                &number.to_string(),
                "-R",
                &self.repo,
                "--json",
                "url,state,baseRefName,baseRefOid,headRefName,headRefOid,mergeable,mergeStateStatus,statusCheckRollup,latestReviews,mergeCommit,mergedAt,mergedBy",
            ],
        )?;
        ensure!(
            view["baseRefName"] == self.base
                && view["headRefName"] == self.head_branch(run)
                && view["headRefOid"] == head,
            "GitHub PR target or candidate changed"
        );
        Ok(view)
    }

    pub fn wait_ci(
        &self,
        ws: &Workspace,
        out: &Path,
        number: u64,
        head: &str,
        run: &str,
    ) -> Result<Value> {
        let deadline = Instant::now() + Duration::from_secs(45 * 60);
        let mut previous = Value::Null;
        loop {
            let pr = self.pr(ws, number, head, run)?;
            ensure!(
                pr["state"] == "OPEN" && pr["baseRefOid"] == self.base_sha,
                "GitHub PR closed or target moved while waiting for CI"
            );
            if pr != previous {
                save(out, "github-ci.json", &pr)?;
                let checks = pr["statusCheckRollup"]
                    .as_array()
                    .context("no GitHub check list")?;
                let status = checks
                    .iter()
                    .map(|c| {
                        format!(
                            "{} {}",
                            c["name"].as_str().unwrap_or("check"),
                            c["conclusion"]
                                .as_str()
                                .filter(|s| !s.is_empty())
                                .or_else(|| c["status"].as_str())
                                .unwrap_or("?")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                println!("GitHub CI: {status}");
                if let Some(acceptance) = ci_ready(&pr)? {
                    return self.checked_workflow(ws, out, head, acceptance).map(|_| pr);
                }
                previous = pr;
            }
            ensure!(
                Instant::now() < deadline,
                "timed out waiting for PR #{number} checks"
            );
            thread::sleep(Duration::from_secs(15));
        }
    }

    fn checked_workflow(
        &self,
        ws: &Workspace,
        out: &Path,
        head: &str,
        acceptance: &Value,
    ) -> Result<()> {
        let prefix = format!("https://github.com/{}/actions/runs/", self.repo);
        let (workflow, job) = field(acceptance, "detailsUrl")?
            .strip_prefix(&prefix)
            .and_then(|tail| tail.split_once("/job/"))
            .context("unexpected GitHub acceptance job URL")?;
        let workflow: u64 = workflow.parse()?;
        let job: u64 = job.parse()?;
        let view = gh(
            &ws.root,
            &[
                "run",
                "view",
                &workflow.to_string(),
                "-R",
                &self.repo,
                "--json",
                "url,headSha,event,status,conclusion,jobs",
            ],
        )?;
        save(out, "github-actions.json", &view)?;
        ensure!(
            view["headSha"] == head
                && view["event"] == "pull_request"
                && view["status"] == "completed"
                && view["conclusion"] == "success",
            "GitHub Actions did not pass for the exact PR head"
        );
        let jobs = view["jobs"].as_array().context("missing workflow jobs")?;
        for name in ["protocol", "desktop", "acceptance"] {
            ensure!(
                jobs.iter()
                    .any(|j| j["name"] == name && j["conclusion"] == "success"),
                "PR workflow did not pass {name}"
            );
        }
        let log = command(
            "gh",
            &ws.root,
            &[
                "run",
                "view",
                &workflow.to_string(),
                "-R",
                &self.repo,
                "--job",
                &job.to_string(),
                "--log",
            ],
        )?;
        ensure!(!log.is_empty(), "GitHub acceptance job log is empty");
        fs::write(out.join("github-acceptance.log"), log)?;
        Ok(())
    }

    pub fn retire_dispatch(&self, ws: &Workspace) -> Result<()> {
        let dispatch = format!("refs/heads/aep-task/{}", self.task);
        if remote_ref(ws, &dispatch)?.is_empty() {
            return Ok(());
        }
        git(
            &ws.root,
            &[
                "push",
                "--no-follow-tags",
                "--recurse-submodules=no",
                &format!("--force-with-lease={dispatch}:{}", self.spec),
                "origin",
                &format!(":{dispatch}"),
            ],
        )?;
        ensure!(
            remote_ref(ws, &dispatch)?.is_empty(),
            "test dispatch branch remains on GitHub"
        );
        Ok(())
    }
}

pub fn remote_ref(ws: &Workspace, reference: &str) -> Result<String> {
    Ok(
        git(&ws.root, &["ls-remote", "--refs", "origin", reference])?
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .into(),
    )
}

fn ci_ready(pr: &Value) -> Result<Option<&Value>> {
    Ok(ready_checks(pr)?.and_then(|checks| {
        checks
            .iter()
            .find(|c| c["name"] == "acceptance" && c["conclusion"] == "SUCCESS")
    }))
}

fn ready_checks(pr: &Value) -> Result<Option<&[Value]>> {
    let checks = pr["statusCheckRollup"]
        .as_array()
        .context("missing CI status")?;
    let mut all_passed = true;
    let mut passed = false;
    for check in checks {
        let conclusion = match check["__typename"].as_str() {
            Some("CheckRun") if check["status"] == "COMPLETED" => check["conclusion"].as_str(),
            Some("StatusContext") => check["state"].as_str(),
            _ => None,
        };
        match conclusion {
            Some("SUCCESS") => passed = true,
            Some("SKIPPED" | "NEUTRAL") => {}
            None | Some("" | "PENDING" | "EXPECTED") => all_passed = false,
            Some(failure) => bail!(
                "GitHub CI failed for {}: {failure}",
                check["name"]
                    .as_str()
                    .or(check["context"].as_str())
                    .unwrap_or("check")
            ),
        }
    }
    if !all_passed
        || !passed
        || pr["mergeable"] != "MERGEABLE"
        || !matches!(pr["mergeStateStatus"].as_str(), Some("CLEAN" | "HAS_HOOKS"))
    {
        return Ok(None);
    }
    Ok(Some(checks))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ci_requires_the_successful_acceptance_check_and_a_mergeable_pr() {
        let mut pr = json!({
            "mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN", "statusCheckRollup": [
                {"__typename":"CheckRun", "name":"desktop", "status":"IN_PROGRESS", "conclusion":""},
                {"__typename":"CheckRun", "name":"acceptance", "status":"COMPLETED", "conclusion":"SKIPPED"}
            ]
        });
        assert!(ci_ready(&pr).unwrap().is_none());
        pr["statusCheckRollup"][0]["status"] = json!("COMPLETED");
        pr["statusCheckRollup"][0]["conclusion"] = json!("SUCCESS");
        assert!(ci_ready(&pr).unwrap().is_none());
        pr["statusCheckRollup"][1]["conclusion"] = json!("SUCCESS");
        assert!(ci_ready(&pr).unwrap().is_some());
        pr["statusCheckRollup"][0]["conclusion"] = json!("FAILURE");
        assert!(ci_ready(&pr).is_err());
    }
}
