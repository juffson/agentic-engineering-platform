//! A confirmed, versioned delivery from one item stage to its immediate successor.
use super::{Project, RecordKind, Work};
use crate::{
    handoff,
    memory::Memory,
    workflow,
    workspace::{Workspace, commit, ident, read, status},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectDecision {
    pub id: i64,
    pub body: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Transfer {
    pub project: String,
    pub work: String,
    pub from: usize,
    pub to: usize,
    pub run: String,
    pub repository: Option<PathBuf>,
    pub candidate: Option<String>,
    /// Managed checkout containing the producing stage’s working tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub project_decisions: Vec<ProjectDecision>,
    pub decision_id: Option<i64>,
    pub decision: String,
    pub acceptance: String,
    pub confirmed_at: String,
}

impl Transfer {
    fn read_workflow(&mut self, home: &Path, work: &Work, a: &super::Attempt) -> Result<()> {
        let from = self.from;
        let run = self.run.as_str();
        ensure!(
            work.current_attempt.get(&from) == Some(&a.id),
            "执行已被新尝试替代"
        );
        let dir = workflow::attempt_dir(home, &self.project, &self.work, run);
        let ev = workflow::evidence(&dir)?;
        ensure!(
            ev["status"] == "succeeded" && ev["run_id"] == run,
            "阶段执行尚未通过"
        );
        let result = fs::read_to_string(dir.join("result.txt"))?;
        self.repository = a.repository.clone();
        if ev["mode"] != "external"
            && let Some(repo) = &a.repository
            && let Some(path) = ev["workspace"].as_str()
        {
            let root = Workspace::open(repo)?.data.join("workflow-worktrees");
            let path = Path::new(path);
            if path.parent() == Some(root.as_path()) {
                self.checkout = path.file_name().and_then(|s| s.to_str()).map(str::to_owned);
            }
        }
        self.decision = handoff::summary(&result);
        let project_store = home.join("workflows").join(&self.project);
        if let Some(db) = Memory::read_only(&project_store)? {
            if let Some(id) = ev["written_memory"].as_i64() {
                let note = db.get(id)?;
                if note["run_id"] == run
                    && let Some(body) = note["body"].as_str()
                {
                    self.decision = handoff::clip(body, 1000);
                }
            }
            if let Some(ids) = ev["read_memory"].as_array() {
                for id in ids.iter().filter_map(|v| v.as_i64()) {
                    let note = db.get(id)?;
                    if let Some(old) = note["replaces"].as_i64() {
                        self.project_decisions.retain(|d| d.id != old);
                    }
                    if note["kind"] == "decision" {
                        self.project_decisions.push(ProjectDecision {
                            id,
                            body: note["body"].as_str().context("缺少项目决策正文")?.into(),
                        });
                    }
                }
            }
        }
        if let Some(upstream) = work.incoming(from)
            && ev["read_stage_transfer"]["run"] == upstream.run
            && upstream.decision_id.is_some()
            && upstream.repository == self.repository
        {
            self.decision_id = upstream.decision_id;
            self.decision = upstream.decision.clone();
        }
        self.acceptance = if matches!(
            work.template.stages()[from].role,
            workflow::Role::Test | workflow::Role::Accept
        ) {
            ensure!(
                result
                    .trim()
                    .lines()
                    .last()
                    .is_some_and(|s| s.trim() == "AEP: PASS"),
                "缺少验收通过回执"
            );
            format!("AEP: PASS · {}", handoff::summary(&result))
        } else {
            "阶段执行成功，操作者已验收交付结果".into()
        };
        Ok(())
    }

    fn read_relay(&mut self, work: &Work) -> Result<()> {
        let from = self.from;
        let run = self.run.as_str();
        let record = work
            .records
            .iter()
            .find(|r| {
                r.kind == RecordKind::Relay
                    && r.stage == Some(from)
                    && run
                        .rsplit_once("-r")
                        .is_some_and(|(prefix, _)| prefix == r.id)
            })
            .context("执行未关联到本事项的来源阶段")?;
        let ws = Workspace::open(&record.repository)?;
        record.kind.verify(&ws, &record.id)?;
        let manifest = read(&ws.data.join("relays").join(&record.id).join("relay.json"))?;
        let attempt = manifest["attempts"]
            .as_array()
            .and_then(|a| a.last())
            .context("接力尚未执行")?;
        ensure!(
            attempt["round"] == "Second" && attempt["id"] == run,
            "请选择最近一次第二轮执行"
        );
        let ev = status(&ws.data, run)?;
        ensure!(
            ev["run_id"] == run
                && ev["status"] == "awaiting_human_review"
                && ev["review_status"] == "findings_recorded"
                && ev["check_exit_code"] == 0
                && ev["blocking_findings"].as_array().is_none_or(Vec::is_empty),
            "接力验收或独立审查尚未通过"
        );
        if let Some(checks) = ev["acceptance"].as_array() {
            ensure!(
                !checks.is_empty()
                    && checks
                        .iter()
                        .all(|c| c["status"] == "passed" && c["exit_code"] == 0),
                "存在未通过的验收项"
            );
        }
        let decision = attempt["decision"].as_i64().context("缺少修订决策")?;
        ensure!(
            ev["request"]["require_memory"] == decision
                && ev["spec_sha"] == manifest["task"]["sha"]
                && ev["task"] == manifest["task"]["name"],
            "验收与接力回执不匹配"
        );
        let note = Memory::read_only(&ws.data)?
            .context("缺少决策库")?
            .get(decision)?;
        ensure!(
            note["kind"] == "decision"
                && note["task"] == ev["task"]
                && note["spec_sha"] == ev["spec_sha"],
            "决策与验收任务不匹配"
        );
        if let Some(t) = ev["read_stage_transfer"].as_object()
            && t.get("project").and_then(|v| v.as_str()) == Some(&self.project)
            && t.get("work").and_then(|v| v.as_str()) == Some(&self.work)
            && let Some(decisions) = t.get("project_decisions")
        {
            self.project_decisions = serde_json::from_value(decisions.clone())?;
        }
        let sha = ev["candidate_sha"].as_str().context("缺少已验收提交")?;
        ensure!(commit(&ws.root, sha)? == sha, "无效的验收提交");
        let review = fs::read_to_string(ws.data.join("runs").join(run).join("review.txt"))?;
        self.repository = Some(ws.root);
        self.candidate = Some(sha.into());
        self.decision_id = Some(decision);
        self.decision = note["body"].as_str().context("缺少决策正文")?.into();
        self.acceptance = format!(
            "验收命令 {} 退出码 0。独立审查通过。{}",
            handoff::clip(&ev["check"].to_string(), 500),
            handoff::clip(&review, 720)
        );
        if let Some(rows) = ev["acceptance"].as_array().filter(|a| a.len() > 1) {
            let checks = rows
                .iter()
                .filter_map(|row| row["id"].as_str())
                .collect::<Vec<_>>()
                .join("、");
            self.acceptance
                .push_str(&format!(" 已通过的验收项：{}", handoff::clip(&checks, 400)));
        }
        Ok(())
    }
    pub fn repository_decision(&self, repo: Option<&Path>) -> Option<i64> {
        self.decision_id
            .filter(|_| repo.is_some() && self.repository.as_deref() == repo)
    }
    pub fn context(&self) -> String {
        let reference = self
            .decision_id
            .map(|n| format!("仓库 #{n} "))
            .unwrap_or_default();
        let mut text = format!(
            "阶段交接 · 第 {} 阶段 → 第 {} 阶段\n来源执行 {}\n已确认决策 · {reference}{}\n验收结果 · {}{}",
            self.from + 1,
            self.to + 1,
            self.run,
            self.decision,
            self.acceptance,
            self.candidate
                .as_ref()
                .map(|sha| format!("\n已验收提交 {sha}"))
                .unwrap_or_default()
        );
        for decision in &self.project_decisions {
            text.push_str(&format!(
                "\n来源阶段采用的项目决策 #{} · {}",
                decision.id, decision.body
            ));
        }
        text
    }
}

impl Work {
    pub fn incoming(&self, stage: usize) -> Option<&Transfer> {
        self.active_transfers
            .get(&stage)
            .and_then(|idx| self.stage_transfers.get(*idx))
    }
    pub fn reopen_transfers(&mut self, stage: usize) {
        self.active_transfers.retain(|to, _| *to <= stage);
    }
}
impl Project {
    /// Advance the current stage with the same receipt rules in Studio and in the CLI.
    pub fn finish_stage(
        &mut self,
        home: &Path,
        wid: &str,
        from: usize,
        mut note: String,
    ) -> Result<()> {
        let pid = self.id.clone();
        let work = self.work_by_id_mut(wid)?;
        ensure!(work.stage == from && from < work.notes.len(), "阶段已变更");
        let has_next = from + 1 < work.template.stages().len();
        let attempt = work.current_attempt.get(&from).cloned();
        if let Some(run) = &attempt {
            let dir = workflow::attempt_dir(home, &pid, wid, run);
            ensure!(
                workflow::evidence(&dir)?["status"] == "succeeded",
                "本阶段执行尚未通过"
            );
            if note.trim().is_empty() {
                note = fs::read_to_string(dir.join("result.txt"))?;
            }
        }
        ensure!(!note.trim().is_empty(), "请先填写阶段结论");
        if !has_next
            && let Some(t) = work.incoming(from)
            && work
                .template
                .stages()
                .get(t.from)
                .is_some_and(|s| s.role == workflow::Role::Accept)
            && let Some(run) = attempt.as_deref()
        {
            let receipt = super::archive::seal(home, &pid, work, run, &note)?;
            if !work.deliveries.iter().any(|d| d.run == run) {
                work.deliveries.push(receipt);
            }
        }
        if let Some(run) = attempt.filter(|_| has_next) {
            self.confirm_transfer(home, wid, from, &run)?;
        } else {
            work.stage += 1;
        }
        self.work_by_id_mut(wid)?.notes[from] = note;
        Ok(())
    }

    pub fn confirm_transfer(
        &mut self,
        home: &Path,
        wid: &str,
        from: usize,
        run: &str,
    ) -> Result<Transfer> {
        ident(run, 64)?;
        let project = self.id.clone();
        let work = self.work_by_id_mut(wid)?;
        ensure!(
            from < work.template.stages().len().saturating_sub(1),
            "没有后续阶段"
        );
        let to = from + 1;
        if let Some(existing) = work.incoming(to).filter(|h| h.run == run) {
            return Ok(existing.clone());
        }
        ensure!(work.stage <= from, "事项已进入下一阶段，请先返回来源阶段");
        if let Some(last) = work.attempts.last() {
            ensure!(
                !handoff::busy(&workflow::evidence(&workflow::attempt_dir(
                    home, &project, wid, &last.id
                ))?),
                "事项已有活动执行"
            );
        }
        let mut transfer = Transfer {
            project: project.clone(),
            work: wid.into(),
            from,
            to,
            run: run.into(),
            repository: None,
            candidate: None,
            checkout: None,
            project_decisions: vec![],
            decision_id: None,
            decision: String::new(),
            acceptance: String::new(),
            confirmed_at: chrono::Utc::now().to_rfc3339(),
        };
        if let Some(a) = work
            .attempts
            .iter()
            .find(|a| a.id == run && a.stage == from)
        {
            transfer.read_workflow(home, work, a)?;
        } else {
            transfer.read_relay(work)?;
        }
        work.reopen_transfers(from);
        work.active_transfers.insert(to, work.stage_transfers.len());
        work.stage_transfers.push(transfer.clone());
        work.stage = to;
        Ok(transfer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        memory::Key,
        project::{Kind, Library, save_json},
        workflow::Template,
        workspace::git,
    };
    use serde_json::json;

    #[test]
    fn only_the_latest_passing_round_can_transfer_its_exact_decision() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path().canonicalize()?;
        git(&root, &["init", "-q"])?;
        git(
            &root,
            &[
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "--allow-empty",
                "-qm",
                "base",
            ],
        )?;
        let ws = Workspace::open(&root)?;
        let sha = commit(&root, "HEAD")?;
        let key = Key::new("sample", &sha, "");
        let decision =
            Memory::open(&ws.data)?.add(&key, "decision", "retain exported API", None)?;
        let record = "relay-source";
        let run = "relay-source-r2-2";
        let relay = ws.data.join("relays").join(record);
        let dir = ws.data.join("runs").join(run);
        fs::create_dir_all(&dir)?;
        fs::create_dir_all(&relay)?;
        fs::write(dir.join("review.txt"), "checks match the spec")?;
        save_json(
            &relay,
            "relay.json",
            &json!({"id":record,"task":{"name":"sample","sha":sha},"attempts":[{"id":run,"round":"Second","decision":decision}]}),
        )?;
        let mut lib = Library::default();
        lib.create("test", vec![root.clone()])?;
        let p = lib.project_mut()?;
        p.add_work("feature", Kind::Feature, Template::Software)?;
        let w = p.selected_mut()?;
        w.repositories = vec![root.clone()];
        let wid = w.id.clone();
        p.link_at_stage(&wid, &root, RecordKind::Relay, record, Some(3))?;
        let mut ev = json!({"run_id":run,"candidate_sha":sha,"spec_sha":sha,"task":"sample","request":{"require_memory":decision},"status":"awaiting_human_review","review_status":"findings_recorded","blocking_findings":[],"check_exit_code":0,"acceptance":[{"id":"behavior","status":"failed","exit_code":1}]});
        save_json(&dir, "evidence.json", &ev)?;
        assert!(p.confirm_transfer(&root, &wid, 3, run).is_err());
        ev["acceptance"][0]["status"] = json!("passed");
        ev["acceptance"][0]["exit_code"] = json!(0);
        save_json(&dir, "evidence.json", &ev)?;
        assert!(p.confirm_transfer(&root, &wid, 2, run).is_err());
        assert!(
            p.confirm_transfer(&root, &wid, 3, "relay-source-r1-1")
                .is_err()
        );
        let receipt = p.confirm_transfer(&root, &wid, 3, run)?;
        assert_eq!(receipt.decision, "retain exported API");
        assert_eq!(receipt.candidate.as_ref(), Some(&sha));
        assert_eq!(p.confirm_transfer(&root, &wid, 3, run)?, receipt);
        let work = p.selected_mut()?;
        assert_eq!(work.stage_transfers.len(), 1);
        assert_eq!(work.incoming(4), Some(&receipt));
        work.reopen_transfers(3);
        assert!(work.incoming(4).is_none());
        assert_eq!(work.stage_transfers[0], receipt);
        Ok(())
    }
}
