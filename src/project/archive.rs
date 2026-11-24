//! Local delivery bundles, committed to the item catalog after their files are complete.
use super::{Work, save_json, timeline};
use crate::{
    workflow,
    workspace::{Workspace, ident, read},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delivery {
    pub project: String,
    pub work: String,
    pub run: String,
    pub accepted_run: String,
    pub archived_at: String,
    pub attempts: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge: Option<super::merge::MergeReceipt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merge_history: Vec<super::merge::AbandonedIntent>,
}
impl Delivery {
    pub fn snapshot(&self, home: &Path) -> Result<timeline::Timeline> {
        for id in [&self.project, &self.work, &self.run] {
            ident(id, 64)?;
        }
        let directory = self.directory(home);
        let manifest = read(&directory.join("manifest.json"))?;
        let snapshot: timeline::Timeline =
            serde_json::from_reader(fs::File::open(directory.join("timeline.json"))?)?;
        let mut original = self.clone();
        original.merge = None;
        original.merge_history.clear();
        ensure!(
            manifest["delivery"] == serde_json::to_value(&original)?
                && snapshot.work_id == self.work
                && snapshot.rounds.len() == self.attempts
                && snapshot.deliveries.contains(&original),
            "归档索引和事项不匹配"
        );
        let mut snapshot = snapshot;
        for delivery in &mut snapshot.deliveries {
            if delivery.run == self.run {
                delivery.merge = super::merge::receipt(home, self)?.or(self.merge.clone());
                delivery.merge_history = self.merge_history.clone();
            }
        }
        snapshot.merge_events = snapshot
            .deliveries
            .iter()
            .filter_map(|d| d.merge.as_ref())
            .filter(|r| r.status == super::merge::MergeStatus::Merged)
            .cloned()
            .collect();
        Ok(snapshot)
    }
    pub fn directory(&self, home: &Path) -> PathBuf {
        home.join("workflows")
            .join(&self.project)
            .join(&self.work)
            .join("deliveries")
            .join(&self.run)
    }
}

pub fn run_directory(index: usize, run: &str) -> PathBuf {
    Path::new("runs").join(format!("{:03}-{run}", index + 1))
}

fn copy_artifact(
    base: &Path,
    name: &str,
    root: &Path,
    relative: &Path,
    files: &mut BTreeMap<PathBuf, u64>,
) -> Result<()> {
    let source = base.join(name);
    if !source.try_exists()? {
        return Ok(());
    }
    ensure!(
        fs::symlink_metadata(&source)?.is_file()
            && source.canonicalize()?.starts_with(base.canonicalize()?),
        "不是普通证据文件 {}",
        source.display()
    );
    let target = root.join(relative);
    fs::create_dir_all(target.parent().context("archive directory")?)?;
    files.insert(relative.into(), fs::copy(source, target)?);
    Ok(())
}

pub(super) fn seal(
    home: &Path,
    project: &str,
    work: &Work,
    delivery: &str,
    note: &str,
) -> Result<Delivery> {
    for id in [project, work.id.as_str(), delivery] {
        ident(id, 64)?;
    }
    let stage = work.stage;
    let accepted = work.incoming(stage).context("缺少独立验收交接")?;
    ensure!(
        accepted.from.checked_add(1) == Some(stage)
            && work
                .template
                .stages()
                .get(accepted.from)
                .is_some_and(|s| s.role == workflow::Role::Accept),
        "来源不是独立验收阶段"
    );
    let dir = workflow::attempt_dir(home, project, &work.id, delivery);
    let approval = workflow::attempt_dir(home, project, &work.id, &accepted.run);
    let accept_ev = workflow::evidence(&approval)?;
    let ev = workflow::evidence(&dir)?;
    ensure!(
        accept_ev["run_id"] == accepted.run
            && accept_ev["status"] == "succeeded"
            && fs::read_to_string(approval.join("result.txt"))?
                .trim()
                .lines()
                .last()
                .is_some_and(|line| line.trim() == "AEP: PASS"),
        "独立验收缺少通过回执"
    );
    ensure!(
        ev["run_id"] == delivery
            && ev["status"] == "succeeded"
            && ev["read_stage_transfer"] == serde_json::to_value(accepted)?,
        "这次交付执行没有记录它读取的独立验收交接（旧版本执行器的复核会这样）。请在「交付归档」继续对话一次（例如“请确认交付”），新一轮通过后再确认交付"
    );

    if !ev["source_evidence"].is_null() {
        ensure!(
            ev["source_evidence"]["run"] == accepted.run
                && fs::read(dir.join("inputs/source-evidence.json"))?
                    == fs::read(approval.join("evidence.json"))?,
            "交付读取的回执与原独立验收回执不一致"
        );
    }
    let mut history = timeline::load(home, project, work)?;
    ensure!(
        history.errors.is_empty(),
        "归档证据不完整：{}",
        history.errors.join("、")
    );
    let receipt = Delivery {
        project: project.into(),
        work: work.id.clone(),
        run: delivery.into(),
        accepted_run: accepted.run.clone(),
        archived_at: chrono::Utc::now().to_rfc3339(),
        attempts: history.rounds.len(),
        merge: None,
        merge_history: vec![],
    };
    let target = receipt.directory(home);
    if target.try_exists()? {
        let saved: Delivery =
            serde_json::from_value(read(&target.join("manifest.json"))?["delivery"].clone())?;
        ensure!(
            saved.project == project
                && saved.work == work.id
                && saved.run == delivery
                && saved.accepted_run == accepted.run,
            "已有归档与交付不匹配"
        );
        saved.snapshot(home)?;
        return Ok(saved);
    }
    let parent = target.parent().context("归档目录")?;
    fs::create_dir_all(parent)?;
    let temp = tempfile::tempdir_in(parent)?;
    let mut files = BTreeMap::new();
    let mut runs = Vec::new();
    for (index, row) in history.rounds.iter().enumerate() {
        ident(&row.run, 64)?;
        let source = if work.attempts.iter().any(|a| a.id == row.run) {
            workflow::attempt_dir(home, project, &work.id, &row.run)
        } else {
            Workspace::open(row.repository.as_deref().context("缺少执行仓库")?)?
                .data
                .join("runs")
                .join(&row.run)
        };
        let relative = run_directory(index, &row.run);
        runs.push(json!({"run":row.run,"record":row.record,"stage_index":row.stage,"status":row.status,"directory":relative}));
        ensure!(
            source.join("evidence.json").is_file(),
            "缺少执行回执 {}",
            row.run
        );
        for name in [
            "evidence.json",
            "request.json",
            "prompt.txt",
            "result.txt",
            "inputs/source-evidence.json",
            "stdout.log",
            "stderr.log",
            "handoff.json",
            "review.txt",
            "checks.log",
            "implementation.log",
            "review-result.json",
            "session.json",
            "zcode-edit/session.json",
            "zcode-plan/session.json",
            "zcode-edit/desktop-task.json",
            "zcode-plan/desktop-task.json",
            "zcode-edit/desktop-approvals.json",
            "zcode-plan/desktop-approvals.json",
            "zcode-edit/zcode-model.json",
            "zcode-plan/zcode-model.json",
            "zcode-review-prompt.txt",
            "zcode-review-result.txt",
            "zcode-review.log",
        ] {
            copy_artifact(&source, name, temp.path(), &relative.join(name), &mut files)?;
        }
        if row.round.is_some() {
            let repo = row.repository.as_deref().context("缺少接力仓库")?;
            let ev = workflow::evidence(&source)?;
            if let (Some(spec), Some(candidate)) =
                (ev["spec_sha"].as_str(), ev["candidate_sha"].as_str())
            {
                ensure!(
                    [spec, candidate]
                        .iter()
                        .all(|sha| matches!(sha.len(), 40 | 64)
                            && sha.bytes().all(|b| b.is_ascii_hexdigit())),
                    "无效的接力提交"
                );
                let bytes = std::process::Command::new("git")
                    .current_dir(repo)
                    .args([
                        "diff",
                        "--binary",
                        "--no-ext-diff",
                        "--no-textconv",
                        spec,
                        candidate,
                        "--",
                    ])
                    .output()?;
                ensure!(bytes.status.success(), "无法归档执行 {} 的变更", row.run);
                let path = relative.join("changes.patch");
                fs::write(temp.path().join(&path), &bytes.stdout)?;
                files.insert(path, bytes.stdout.len() as u64);
            }
        }
    }
    history.deliveries.push(receipt.clone());
    save_json(temp.path(), "timeline.json", &history)?;
    save_json(
        temp.path(),
        "manifest.json",
        &json!({"delivery":receipt,"work_name":work.name,"operator_note":note,"conclusion":crate::handoff::summary(&fs::read_to_string(dir.join("result.txt"))?),"runs":runs,"files":files,"timeline":"timeline.json"}),
    )?;
    fs::rename(temp.path(), &target)?;
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        handoff::Assignment,
        project::{Kind, Library},
        workflow::Template,
    };

    #[test]
    fn only_confirmed_acceptance_can_seal_a_reopenable_delivery() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let home = tmp.path();
        let mut library = Library::default();
        library.create("Demo", vec![])?;
        let p = library.project_mut()?;
        p.add_work("Guide", Kind::Feature, Template::Software)?;
        let wid = p.selected_mut()?.id.clone();
        let accept = 5;
        let deliver = 6;
        p.selected_mut()?.stage = accept;
        let assignment = |w: &Work, stage| Assignment {
            profile: w.profile(stage),
            brief: "verify".into(),
            repository: None,
            external: None,
        };
        let input = assignment(p.selected_mut()?, accept);
        let approval = p.reserve(home, &wid, accept, input)?;
        let upstream = p.selected_mut()?.current_attempt[&accept].clone();
        save_json(
            &approval,
            "evidence.json",
            &json!({"run_id":upstream,"status":"succeeded"}),
        )?;
        fs::write(approval.join("result.txt"), "checked the guide\nAEP: PASS")?;
        let transfer = p.confirm_transfer(home, &wid, accept, &upstream)?;
        let input = assignment(p.selected_mut()?, deliver);
        let delivery = p.reserve(home, &wid, deliver, input)?;
        let run = p.selected_mut()?.current_attempt[&deliver].clone();
        let mut ev = json!({"run_id":run,"status":"succeeded","source_evidence":{"run":upstream,"file":"inputs/source-evidence.json"}});
        save_json(&delivery, "evidence.json", &ev)?;
        fs::write(delivery.join("result.txt"), "Guide delivered")?;
        assert!(p.finish_stage(home, &wid, deliver, String::new()).is_err());
        ev["read_stage_transfer"] = json!(transfer);
        save_json(&delivery, "evidence.json", &ev)?;
        fs::create_dir(delivery.join("inputs"))?;
        let pinned = delivery.join("inputs/source-evidence.json");
        fs::write(&pinned, "{}")?;
        assert!(p.finish_stage(home, &wid, deliver, String::new()).is_err());
        assert!(p.selected_mut()?.deliveries.is_empty());
        assert_eq!(p.selected_mut()?.stage, deliver);
        fs::copy(approval.join("evidence.json"), &pinned)?;
        let pid = p.id.clone();
        let sealed = seal(home, &pid, p.selected_mut()?, &run, "archived")?;
        p.finish_stage(home, &wid, deliver, String::new())?;
        assert_eq!(
            p.selected_mut()?.deliveries.as_slice(),
            std::slice::from_ref(&sealed)
        );
        fs::write(approval.join("result.txt"), "later edit")?;
        let archived = sealed.directory(home).join(run_directory(0, &upstream));
        assert_eq!(
            fs::read_to_string(archived.join("result.txt"))?,
            "checked the guide\nAEP: PASS"
        );
        let snapshot = sealed.snapshot(home)?;
        assert_eq!(snapshot.rounds.len(), 2);
        assert_eq!(snapshot.stage_transfers, [transfer]);
        Ok(())
    }
}
