mod github;
use aep::workspace::{Workspace, git, read};
use anyhow::{Context, Result, ensure};
use github::command;
pub(crate) use github::{Branch, Choices, Draft, Plan, choices};
pub use github::{Fixture, remote_ref};
use serde::Serialize;
use serde_json::Value;
use std::{fs, path::Path};

pub fn archive(root: &Path, out: &Path) -> Result<()> {
    let repo = root.join("workspace/repo");
    if repo.join(".git").is_dir() {
        let ws = Workspace::open(&repo)?;
        let runs = ws.data.join("runs");
        if runs.is_dir() {
            let ids = fs::read_dir(runs)?
                .map(|entry| {
                    let name = entry?.file_name();
                    name.into_string()
                        .map_err(|_| anyhow::anyhow!("invalid Run directory"))
                })
                .collect::<Result<Vec<_>>>()?;
            export_runs(&ws, &out.join("runs"), ids.iter().map(String::as_str))?;
        }
        let relays = ws.data.join("relays");
        if relays.is_dir() {
            for entry in fs::read_dir(relays)? {
                let source = entry?.path().join("relay.json");
                if source.is_file() {
                    fs::copy(source, out.join("relay.json"))?;
                }
            }
        }
        let memory = ws.data.join("memory.sqlite3");
        if memory.is_file() {
            fs::copy(memory, out.join("memory.sqlite3"))?;
        }
    }
    let mut shots = fs::read_dir(out)?
        .filter_map(|entry| {
            let path = entry.ok()?.file_name().into_string().ok()?;
            path.ends_with(".png").then_some(path)
        })
        .collect::<Vec<_>>();
    shots.sort();
    let mut index = String::from("# Studio live lifecycle evidence\n\n");
    for (file, label) in [
        ("verification.json", "Verification"),
        ("spec.md", "Task Spec"),
        ("task.json", "Task manifest"),
        ("dispatch.json", "Dispatch receipt"),
        ("failure.txt", "Failure"),
        ("relay-verification.json", "Relay and memory reads"),
        ("reviewed.diff", "Reviewed diff"),
        ("github-ci.json", "GitHub checks"),
        ("github-actions.json", "GitHub Actions jobs"),
        ("github-acceptance.log", "GitHub acceptance log"),
        ("github-merged.json", "GitHub merge"),
        ("runs", "Execution evidence and Studio receipts"),
    ] {
        if out.join(file).exists() {
            index.push_str(&format!("- [{label}]({file})\n"));
        }
    }
    index.push_str("\n## Studio screenshots\n\n");
    for file in shots {
        index.push_str(&format!("- [{file}]({file})"));
        let controls = format!("{}.json", file.trim_end_matches(".png"));
        if out.join(&controls).is_file() {
            index.push_str(&format!(" · [controls]({controls})"));
        }
        index.push('\n');
    }
    fs::write(out.join("index.md"), index)?;
    let archive = tempfile::NamedTempFile::new_in(root)?;
    command(
        "tar",
        root,
        &[
            "-czf",
            archive.path().to_str().context("archive path")?,
            "evidence",
        ],
    )?;
    archive.persist(root.join("evidence.tar.gz"))?;
    Ok(())
}

pub fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("missing {key}"))
}

pub fn save(out: &Path, name: &str, data: &impl Serialize) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(out)?;
    serde_json::to_writer_pretty(&mut file, data)?;
    file.persist(out.join(name))?;
    Ok(())
}

pub fn verify_reference(ws: &Workspace, run: &str, next: &Value, reference: &str) -> Result<()> {
    let folder = ws.data.join("runs").join(run);
    let handoff = read(&folder.join("handoff.json"))?;
    let review = read(&folder.join("review-result.json"))?;
    ensure!(
        handoff["note"]
            .as_str()
            .is_some_and(|s| s.contains(reference))
            && handoff["question"] == "",
        "Codex did not acknowledge the decision reference and close the question"
    );
    ensure!(
        review["structured_output"]["report"]
            .as_str()
            .is_some_and(|s| s.contains(reference)),
        "Claude's report did not quote the decision reference"
    );
    let spec = next["spec_sha"].as_str().context("spec SHA")?;
    let candidate = next["candidate_sha"].as_str().context("candidate SHA")?;
    let diff = git(&ws.root, &["diff", "--no-ext-diff", spec, candidate])?;
    let spec_path = format!(
        "{spec}:specs/{}/spec.md",
        next["task"].as_str().context("task")?
    );
    let spec_body = git(&ws.root, &["show", &spec_path])?;
    ensure!(
        !diff.contains(reference) && !spec_body.contains(reference),
        "decision reference also appeared in the spec or code diff"
    );
    Ok(())
}

pub fn export_runs<'a>(
    ws: &Workspace,
    dest: &Path,
    ids: impl IntoIterator<Item = &'a str>,
) -> Result<()> {
    for id in ids {
        let source = ws.data.join("runs").join(id);
        if !source.exists() {
            continue;
        }
        let output = dest.join(id);
        fs::create_dir_all(&output)?;
        for file in [
            "evidence.json",
            "handoff.json",
            "checks.log",
            "implementation.log",
            "review.txt",
            "review.log",
            "review-result.json",
            "publication.json",
            "merge.json",
            "cleanup.json",
        ] {
            let path = source.join(file);
            if path.try_exists()? {
                fs::copy(path, output.join(file))?;
            }
        }
    }
    Ok(())
}

use crate::{Runner, cleanup, publish, relay};
use aep::workspace::{commit, status};
use serde_json::json;
use std::path::PathBuf;

pub(crate) fn root(ws: &Workspace) -> Option<&Path> {
    let workspace = ws.root.parent()?;
    let root = workspace.parent()?;
    (ws.root.file_name()? == "repo" && workspace.file_name()? == "workspace").then_some(root)
}

pub(crate) fn load(ws: &Workspace) -> Option<Fixture> {
    load_session(root(ws)?)
}

pub(crate) fn load_session(root: &Path) -> Option<Fixture> {
    let path = root.join("evidence/fixture.json");
    let test: Fixture = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    (test.studio
        && (test.real_task
            || (test.task.starts_with("studio-lifecycle-")
                && test.base == format!("test/{}", test.task))))
    .then_some(test)
}

impl Fixture {
    pub(crate) fn guard(&self, target: &publish::Target) -> Result<()> {
        ensure!(
            (self.real_task || self.base == format!("test/{}", self.task))
                && self.task == target.task
                && self.spec == target.spec
                && self.base == target.base
                && self.base_sha == target.base_sha
                && self.repo.eq_ignore_ascii_case(&target.repo)
                && self.head_branch(&target.run) == target.head,
            "候选与隔离测试的任务或目标分支不一致"
        );
        Ok(())
    }

    pub(crate) fn review(&self, ws: &Workspace, target: &publish::Target) -> Result<()> {
        self.guard(target)?;
        let out = root(ws)
            .context("missing isolated test directory")?
            .join("evidence");
        let relay = relay::load_one(ws, &target.relay)?;
        let first = relay
            .latest(relay::Round::First)
            .context("missing first round")?;
        let second = relay
            .latest(relay::Round::Second)
            .context("missing second round")?;
        ensure!(second.id == target.run, "候选不是当前第二轮");
        let first_run = status(&ws.data, &first.id)?;
        let next = status(&ws.data, &second.id)?;
        let decision_id = second.decision.context("missing second round decision")?;
        ensure!(
            first_run["spec_sha"] == self.spec
                && next["spec_sha"] == self.spec
                && first_run["candidate_sha"] == next["requested_candidate_sha"]
                && next["request"]["require_memory"] == decision_id
                && next["implementation_source"] == "codex"
                && next["reviewer"] == "claude",
            "第二轮与固定的候选、决策或客户端不一致"
        );
        let snapshot = aep::studio::Snapshot::load(ws);
        let decision = snapshot
            .notes
            .iter()
            .find(|n| n["id"] == decision_id && n["kind"] == "decision" && n["author"] == "manual")
            .context("missing manual decision")?;
        ensure!(
            decision["task"] == self.task
                && decision["spec_sha"] == self.spec
                && decision["scope"] == "task",
            "人工决策与本次任务不匹配"
        );
        let mut parent = decision;
        let mut question = None;
        while let Some(previous) = parent["replaces"].as_i64() {
            ensure!(
                Some(previous) < parent["id"].as_i64(),
                "invalid revision chain"
            );
            parent = snapshot
                .notes
                .iter()
                .find(|n| n["id"] == previous)
                .context("missing earlier memory version")?;
            if parent["kind"] == "open_question" && parent["run_id"] == first.id {
                question = Some(parent);
                break;
            }
        }
        ensure!(
            self.real_task || question.is_some(),
            "需通过 Studio 将第一轮的问题修订为决策"
        );
        for role in ["implementer", "reviewer"] {
            let inputs = next["memory_inputs"][role]
                .as_array()
                .context("missing shared memory inputs")?;
            ensure!(
                inputs.contains(&json!(decision_id))
                    && question.is_none_or(|q| !inputs.contains(&q["id"])),
                "{role} 未读取修订后的决策"
            );
        }
        let reference = if self.real_task {
            aep::workspace::validate_task_target(&ws.root, &self.base_sha, &self.spec, &self.task)?;
            ensure!(
                target.merge_base == self.base_sha,
                "候选与已选择的目标分支不兼容"
            );
            None
        } else {
            let (_, reference) = field(decision, "body")?
                .rsplit_once("Decision reference ")
                .context("决策缺少 Studio 核验标识")?;
            let reference = reference.trim_end_matches('.');
            ensure!(
                reference.starts_with("decision-") && reference.len() == 41,
                "invalid decision reference"
            );
            verify_reference(ws, &second.id, &next, reference)?;
            let names = git(
                &ws.root,
                &[
                    "diff",
                    "--name-only",
                    "--no-renames",
                    &self.base_sha,
                    &target.candidate,
                ],
            )?;
            let expected = ["acceptance.rs", "normalize.rs", "spec.md", "task.json"]
                .map(|file| format!("specs/{}/{file}", self.task));
            ensure!(
                names.lines().eq(expected.iter().map(String::as_str)),
                "最终 Diff 包含测试任务之外的文件"
            );
            Some(reference)
        };
        fs::write(
            out.join("reviewed.diff"),
            git(
                &ws.root,
                &[
                    "diff",
                    "--no-color",
                    "--no-ext-diff",
                    "--no-textconv",
                    &self.base_sha,
                    &target.candidate,
                    "--",
                ],
            )?,
        )?;
        if let Some(question) = question {
            save(&out, "question.json", question)?;
        }
        save(&out, "decision.json", decision)?;
        save(
            &out,
            "relay-verification.json",
            &json!({"first_run":first.id,"second_run":second.id,
            "candidate":target.candidate,"spec_sha":self.spec,"decision":decision_id,"question":question.map(|q|&q["id"]),
            "reference":reference,"memory_inputs":next["memory_inputs"]}),
        )
    }

    pub(crate) fn finish(
        &self,
        ws: &Workspace,
        runner: &Runner,
        publication: &publish::Publication,
    ) -> Result<cleanup::Receipt> {
        let target = &publication.target;
        self.guard(target)?;
        let out = root(ws)
            .context("missing isolated test directory")?
            .join("evidence");
        let relay = relay::load_one(ws, &target.relay)?;
        let first = &relay
            .latest(relay::Round::First)
            .context("missing first round")?
            .id;
        let preserved = [
            ws.data.join("runs").join(first).join("evidence.json"),
            ws.data.join("runs").join(&target.run).join("evidence.json"),
            ws.data.join("memory.sqlite3"),
            ws.data.join("runs").join(&target.run).join("merge.json"),
        ];
        let before = preserved
            .iter()
            .map(fs::read)
            .collect::<std::io::Result<Vec<_>>>()?;
        let receipt = cleanup::finish(ws, runner, publication)?;
        if !receipt.complete() {
            return Ok(receipt);
        }
        let merged = self.pr(ws, receipt.pr, &target.candidate, &target.run)?;
        let synced_tip = commit(&ws.root, "HEAD")?;
        if self.real_task {
            git(
                &ws.root,
                &[
                    "merge-base",
                    "--is-ancestor",
                    &receipt.merged_sha,
                    &synced_tip,
                ],
            )
            .context("本地同步后的目标没有包含本次合并")?;
        } else {
            ensure!(
                synced_tip == receipt.merged_sha
                    && remote_ref(ws, &format!("refs/heads/{}", self.base))? == receipt.merged_sha,
                "隔离验证分支与合并提交不一致"
            );
        }
        ensure!(
            merged["state"] == "MERGED"
                && merged["mergeCommit"]["oid"] == receipt.merged_sha
                && remote_ref(ws, &format!("refs/heads/{}", target.head))?.is_empty(),
            "候选清理与 GitHub 合并记录不一致"
        );
        for id in [first, &target.run] {
            let record = status(&ws.data, id)?;
            ensure!(
                receipt
                    .worktrees
                    .iter()
                    .any(|entry| &entry.run == id && entry.step.done)
                    && !Path::new(field(&record, "worktree")?).try_exists()?,
                "临时工作区尚未清理"
            );
        }
        for (path, bytes) in preserved.iter().zip(before) {
            ensure!(
                fs::read(path)? == bytes,
                "收尾修改了历史证据 {}",
                path.display()
            );
        }
        if !self.real_task {
            self.retire_dispatch(ws)?;
        }
        save(&out, "github-merged.json", &merged)?;
        save(
            &out,
            "verification.json",
            &json!({"status":"passed","target":self.base,"pr":receipt.pr,
            "url":merged["url"],"candidate":target.candidate,"merge_sha":receipt.merged_sha,"synced_target_sha":synced_tip,
            "local_target_synced":true,"candidate_branch_removed":true,"run_worktrees_removed":true,
            "evidence_memory_and_receipts_preserved":true,
            "temporary_task_dispatch_removed":!self.real_task,"real_task":self.real_task}),
        )?;
        Ok(receipt)
    }
}

pub(crate) fn output(ws: &Workspace) -> Option<PathBuf> {
    Some(root(ws)?.join("evidence"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn studio_lifecycle_rejects_a_different_target() {
        let fixture = Fixture {
            source: PathBuf::new(),
            studio: true,
            real_task: false,
            authored: false,
            repo: "user/project".into(),
            task: "studio-lifecycle-abc".into(),
            base: "test/studio-lifecycle-abc".into(),
            base_sha: "base".into(),
            spec: "spec".into(),
            actor: "user".into(),
        };
        let mut target = publish::Target {
            relay: "relay".into(),
            run: "second".into(),
            task: fixture.task.clone(),
            repo: fixture.repo.clone(),
            base: fixture.base.clone(),
            base_sha: fixture.base_sha.clone(),
            merge_base: "base".into(),
            candidate: "candidate".into(),
            spec: fixture.spec.clone(),
            head: fixture.head_branch("second"),
        };
        assert!(fixture.guard(&target).is_ok());
        target.base = "main".into();
        assert!(fixture.guard(&target).is_err());
        target.base = fixture.base.clone();
        target.base_sha = "moved".into();
        assert!(fixture.guard(&target).is_err());

        let mut real = fixture;
        real.real_task = true;
        real.base = "release/stable".into();
        target.base = real.base.clone();
        target.base_sha = real.base_sha.clone();
        assert!(real.guard(&target).is_ok());
        target.spec = "another-spec".into();
        assert!(real.guard(&target).is_err());
    }
}
