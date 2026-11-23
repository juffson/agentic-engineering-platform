//! Compact stage context and shared reservations for both Studio and independent clients.
use crate::{
    cancel::Cancellation,
    project::{Attempt, Project, Work, save_json},
    workflow::{self, ExternalSession, Profile, Request, Role},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
};

pub fn busy(state: &Value) -> bool {
    matches!(
        state["status"].as_str(),
        Some("claimed" | "running" | "initializing")
    )
}
pub fn clip(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.into();
    }
    let mut n = limit.saturating_sub('…'.len_utf8());
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    format!("{}…", &s[..n])
}
pub fn summary(result: &str) -> String {
    let explicit = result
        .lines()
        .rev()
        .find_map(|s| s.trim().strip_prefix("AEP-HANDOFF:"))
        .map(str::trim)
        .filter(|s| !s.is_empty());
    clip(explicit.unwrap_or(result.trim()), 720)
}
pub struct Packet {
    pub more: bool,
    pub text: String,
    pub project_ids: Vec<i64>,
    pub repo_ids: Vec<i64>,
}
fn entries(
    notes: &[Value],
    mut budget: usize,
    each: usize,
    since: Option<i64>,
) -> (String, Vec<i64>) {
    let mut text = String::new();
    let mut ids = vec![];
    for note in notes {
        let id = note["id"].as_i64().unwrap_or_default();
        if since.is_some_and(|cursor| id <= cursor) {
            continue;
        }
        let replaces = note["replaces"]
            .as_i64()
            .map(|n| format!(" (替代 #{n})"))
            .unwrap_or_default();
        let header = format!("\n#{id} {}{replaces} ", note["kind"].as_str().unwrap_or(""));
        if budget < header.len() + 40 {
            break;
        }
        let body = clip(
            note["body"].as_str().unwrap_or(""),
            each.min(budget - header.len()),
        );
        budget -= header.len() + body.len();
        text.push_str(&header);
        text.push_str(&body);
        ids.push(id);
    }
    (text, ids)
}
pub fn packet(
    req: &Request,
    project: &[Value],
    repo: &[Value],
    since: Option<i64>,
) -> Result<Packet> {
    let stage = req.template.stages().get(req.stage).context("无效阶段")?;
    let total = project.len();
    let (project, mut project_ids) = entries(project, 1600, 720, since);
    if since.is_some() {
        return Ok(Packet {
            more: project_ids.len() < total,
            text: project.trim_start().to_owned(),
            project_ids,
            repo_ids: vec![],
        });
    }
    let (repo, mut repo_ids) = entries(repo, 650, 420, None);
    let mut text = format!(
        "AEP Studio 项目：{} / {}\n阶段：{}（{}）\n阶段职责：{}\n本次说明：{}",
        clip(&req.project, 160),
        clip(&req.work, 320),
        stage.name,
        req.template.label(),
        stage.brief,
        clip(req.brief.trim(), 850)
    );
    if let Some(transfer) = &req.stage_transfer {
        text.push('\n');
        text.push_str(&transfer.context());
        for decision in &transfer.project_decisions {
            if !project_ids.contains(&decision.id) {
                project_ids.push(decision.id);
            }
        }
        if let Some(id) = transfer.repository_decision(req.repository.as_deref())
            && !repo_ids.contains(&id)
        {
            repo_ids.insert(0, id);
        }
    }
    let mut remaining = 850usize;
    for h in req.handoffs.iter().rev() {
        if remaining < 120 {
            break;
        }
        let line = clip(
            &format!(
                "\n事项交接 · {} / {}：{}",
                clip(&h.name, 90),
                h.stage,
                h.note
            ),
            remaining.min(480),
        );
        remaining -= line.len();
        text.push_str(&line);
    }
    let mut left = 850;
    for (label, note) in req
        .approved
        .iter()
        .rev()
        .filter(|(_, s)| !s.trim().is_empty())
    {
        if left < 80 {
            break;
        }
        let n = clip(note.trim(), left.min(340));
        if text.contains(&n) || project.contains(&n) {
            continue;
        }
        text.push_str(&format!("\n已确认 · {}：{n}", clip(label, 60)));
        left = left.saturating_sub(n.len() + 1);
    }
    if !project.is_empty() {
        text.push_str(&format!("\n项目记忆{project}"));
    }
    if !repo.is_empty() {
        text.push_str(&format!("\n仓库记忆{repo}"));
    }
    if let Some((_, id)) = req.prior_runs.last() {
        text.push_str(&format!(
            "\n前序执行 {id}。需要完整交付物、命令或文件时使用 aep project show --attempt <ID>。"
        ));
    }
    text.push_str("\n以当前决策和人工结论为准。完整说明存于 request.json。");
    if stage.role != Role::Build {
        text.push_str("保留已有实现。");
    }
    text.push_str("不要推送、合并或发布。用 AEP-HANDOFF: 单行写出结论、证据位置和下一步。");
    if matches!(stage.role, Role::Test | Role::Accept) {
        text.push_str("最后单独一行写 AEP: PASS 或 AEP: BLOCKED，缺必要证据时标为 BLOCKED。");
    }
    if !req.profile.check_command.is_empty() {
        text.push_str(&format!(
            "\n已允许的精确验收命令：{}",
            req.profile.check_command
        ));
    }
    Ok(Packet {
        more: false,
        text,
        project_ids,
        repo_ids,
    })
}
pub struct Assignment {
    pub profile: Profile,
    pub brief: String,
    pub repository: Option<PathBuf>,
    pub external: Option<ExternalSession>,
}
impl Work {
    pub fn request(&self, project: &str, stage: usize, assignment: Assignment) -> Request {
        Request {
            follow_up: None,
            stage_transfer: self.incoming(stage).cloned(),
            handoffs: self.handoffs.clone(),
            project: project.to_owned(),
            work: self.name.clone(),
            template: self.template,
            stage,
            profile: assignment.profile,
            repository: assignment.repository.clone(),
            ticket: assignment.repository.as_ref().map(|p| self.ticket(p)),
            brief: assignment.brief,
            approved: self
                .template
                .stages()
                .iter()
                .zip(&self.notes)
                .take(stage)
                .map(|(s, n)| (s.name.to_owned(), n.clone()))
                .collect(),
            prior_runs: self
                .current_attempt
                .range(..stage)
                .map(|(s, id)| (*s, id.clone()))
                .collect(),
            external: assignment.external,
        }
    }
}
impl Project {
    pub fn work_by_id_mut(&mut self, id: &str) -> Result<&mut Work> {
        self.work
            .iter_mut()
            .find(|w| w.id == id)
            .context("事项不存在")
    }
    pub fn reserve(
        &mut self,
        catalog: &Path,
        work_id: &str,
        stage: usize,
        assignment: Assignment,
    ) -> Result<PathBuf> {
        let Assignment {
            profile,
            brief,
            repository,
            external,
        } = assignment;
        if external.is_none() {
            profile.validate()?;
        }
        let project = self.name.clone();
        let pid = self.id.clone();
        let item = self.work_by_id_mut(work_id)?;
        ensure!(
            stage == item.stage && stage < item.template.stages().len(),
            "当前阶段已改变或事项已完成"
        );
        if let Some(a) = item.attempts.last() {
            let dir = workflow::attempt_dir(catalog, &pid, work_id, &a.id);
            let state = workflow::evidence(&dir)?;
            if busy(&state) {
                if external.is_some() && state["status"] == "claimed" {
                    let existing: Request =
                        serde_json::from_reader(fs::File::open(dir.join("request.json"))?)?;
                    if existing.external == external
                        && existing.profile.client == profile.client
                        && existing.profile.model == profile.model
                    {
                        return Ok(dir);
                    }
                }
                anyhow::bail!("此事项已有活动执行 {}", a.id)
            }
        }
        ensure!(
            repository
                .as_ref()
                .is_none_or(|r| item.repositories.contains(r)),
            "仓库未关联到此事项"
        );
        let id = uuid::Uuid::new_v4().simple().to_string();
        let dir = workflow::attempt_dir(catalog, &pid, work_id, &id);
        let request = item.request(
            &project,
            stage,
            Assignment {
                profile: profile.clone(),
                repository: repository.clone(),
                brief: brief.clone(),
                external,
            },
        );
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)?;
        save_json(&dir, "request.json", &request)?;
        if let Some(session) = &request.external {
            save_json(
                &dir,
                "evidence.json",
                &json!({"request":"request.json","run_id":id,"attempt":id,"status":"claimed","mode":"external","session":session.session,
                "workspace":session.workspace,"profile":profile,"stage":item.template.stages()[stage].name,"started_at":chrono::Utc::now().to_rfc3339()}),
            )?;
        }
        item.notes[stage] = brief;
        if request.external.is_none() {
            item.profiles.insert(stage, profile.clone());
            if let Some(repo) = &repository {
                item.targets.insert(stage, repo.clone());
            }
        }
        item.attempts.push(Attempt {
            id: id.clone(),
            stage,
            profile,
            repository,
        });
        item.current_attempt.insert(stage, id);
        Ok(dir)
    }
    /// Reserve a turn that continues the stage's current attempt in the same CLI session and
    /// workspace. The stage note stays as written; only the new message is sent.
    pub fn reserve_follow_up(
        &mut self,
        catalog: &Path,
        work_id: &str,
        stage: usize,
        message: &str,
    ) -> Result<PathBuf> {
        let message = message.trim();
        ensure!(!message.is_empty(), "请输入要继续发送的消息");
        let pid = self.id.clone();
        let item = self.work_by_id_mut(work_id)?;
        let previous = item
            .current_attempt
            .get(&stage)
            .cloned()
            .context("此阶段还没有可以继续的执行")?;
        let attempt = item
            .attempts
            .iter()
            .find(|a| a.id == previous)
            .cloned()
            .context("找不到上一轮执行")?;
        let prior = workflow::evidence(&workflow::attempt_dir(catalog, &pid, work_id, &previous))?;
        ensure!(
            prior["mode"] != "external",
            "独立会话领取的执行请在原会话中继续"
        );
        ensure!(
            prior["workspace"].is_string(),
            "上一轮执行没有生成工作区，请重新执行"
        );
        let note = item.notes.get(stage).cloned().unwrap_or_default();
        let dir = self.reserve(
            catalog,
            work_id,
            stage,
            Assignment {
                profile: attempt.profile,
                brief: message.to_owned(),
                repository: attempt.repository,
                external: None,
            },
        )?;
        let item = self.work_by_id_mut(work_id)?;
        item.notes[stage] = note;
        let mut request: Request =
            serde_json::from_reader(fs::File::open(dir.join("request.json"))?)?;
        request.follow_up = Some(workflow::FollowUp {
            attempt: previous,
            message: message.to_owned(),
        });
        save_json(&dir, "request.json", &request)?;
        Ok(dir)
    }
}
pub fn cancel(dir: &Path) -> Result<()> {
    let work = dir
        .parent()
        .and_then(Path::parent)
        .context("invalid attempt")?;
    let stop = Cancellation::new(
        work,
        dir.file_name()
            .and_then(|s| s.to_str())
            .context("invalid attempt")?,
    )?;
    let req: Request = serde_json::from_reader(fs::File::open(dir.join("request.json"))?)?;
    if req.external.is_none() {
        stop.request()?;
        return Ok(());
    }
    let _lock = stop.lock()?;
    let mut e = workflow::evidence(dir)?;
    if e["status"] == "claimed" {
        e["status"] = json!("cancelled");
        e["finished_at"] = json!(chrono::Utc::now().to_rfc3339());
        save_json(dir, "evidence.json", &e)?;
    }
    Ok(())
}
