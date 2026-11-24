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
/// Reading convention appended to every stage prompt (R5): locate first, read ranges, cap output.
pub const READING: &str = "读取约定：先用 rg 或 codegraph 定位，再按行范围读取（Read 的 offset/limit 或 sed -n 'a,bp'）；不要 cat 整个大文件；命令输出可能很长时用 head/tail 截断，或写到文件后按需读取。";
/// Project / stage / duty / brief, plus the incoming stage transfer (it may carry human verdicts).
pub fn header(req: &Request) -> Result<String> {
    let stage = req.template.stages().get(req.stage).context("无效阶段")?;
    let mut text = format!(
        "AEP Studio 项目：{} / {}\n阶段：{}（{}）\n阶段职责：{}\n本次说明：{}",
        clip(&req.project, 160),
        clip(&req.work, 320),
        stage.name,
        req.template.label(),
        stage.brief,
        // The person's own instructions for this run: keep them whole (the note size limit).
        clip(req.brief.trim(), 8000)
    );
    if let Some(transfer) = &req.stage_transfer {
        text.push('\n');
        text.push_str(&transfer.context());
    }
    Ok(text)
}
/// Work hand-offs, approved stage notes, project and repository memory and the prior-run line.
/// `header` is only read to skip approved notes it already quotes.
pub fn memory_body(req: &Request, header: &str, project: &str, repo: &str) -> String {
    let mut text = String::new();
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
        if header.contains(&n) || text.contains(&n) || project.contains(&n) {
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
    if req.subtask.is_some() && !req.subtasks_done.is_empty() {
        text.push_str("\n本阶段已完成的子任务（改动已提交到事项分支，不要重做；需要细节用 aep project show --attempt <ID>）：");
        for d in &req.subtasks_done {
            text.push_str(&format!(
                "\n- {} {}（{}）：{}",
                d.id,
                clip(d.title.trim(), 80),
                d.run,
                clip(d.summary.trim(), 600)
            ));
        }
    }
    text
}
/// Stage rules: decision precedence, commit policy, AEP-HANDOFF, specs ownership, subtask scope,
/// optional/send-back markers, gate verdict and the allowed check command.
pub fn rules(req: &Request) -> Result<String> {
    let stage = req.template.stages().get(req.stage).context("无效阶段")?;
    let mut text = String::from("\n以当前决策和人工结论为准。完整说明存于 request.json。");
    if stage.role != Role::Build {
        text.push_str("保留已有实现。");
    }
    text.push_str("不要推送、合并或发布，也不要自己 git commit：");
    text.push_str(match stage.role {
        Role::Plan | Role::Build => "本阶段通过后，平台会把工作区改动自动提交到事项分支。",
        Role::Deliver => "开发产出已由平台提交到事项分支，独立验收针对的就是这个提交；本阶段只核对和汇总，不改文件，合并由人在 Studio「查看 Diff 并合入」完成，不要因为未合并而退回。",
        _ => "本阶段只核对，不改动工作区文件；验收针对的是平台自动提交的当前 HEAD。",
    });
    text.push_str("用 AEP-HANDOFF: 单行写出结论、证据位置和下一步。");
    text.push_str(if stage.role == Role::Plan {
        "\n本阶段负责规格文档：只写 specs/<任务名>/ 下的文件（前序已建目录就沿用，否则用简短英文 kebab-case 新建），不改代码。"
    } else {
        "\n规格文档（specs/）由需求到技术方案阶段负责，本阶段不要新建或改写；报告、命令和证据写在答复里，Studio 会归档。方案需要调整时用 AEP-BACK 交回技术方案。"
    });
    // The last planning stage before the build splits the work into small, checkable subtasks.
    let stages = req.template.stages();
    if stage.role == Role::Plan
        && stages[req.stage + 1..]
            .iter()
            .take_while(|s| s.role != Role::Build)
            .all(|s| s.role != Role::Plan)
        && stages[req.stage + 1..]
            .iter()
            .any(|s| s.role == Role::Build)
    {
        text.push_str("\n在 task.json 里用 subtasks 拆分开发：[{\"id\":\"T1\",\"title\":\"…\",\"paths\":[\"可改的文件或目录\"],\"criteria\":[\"覆盖的验收项\"],\"check\":[\"能单独验证它的命令\"]}]，按依赖顺序排列，每个子任务一个会话内能完成并单独验证。paths 只写仓库里真实存在的文件或目录，要新建的写到父目录已存在的那一级；平台会在本阶段结束时核对，找不到的路径会让本阶段不通过。");
    }
    if let Some(task) = &req.subtask {
        text.push_str(&format!(
            "\n本轮只实现子任务 {} {}：可改范围 {}；覆盖 {}；完成后运行 {} 并在答复里给出结果。其它子任务留给后续会话。",
            task.id,
            task.title,
            if task.paths.is_empty() { "按方案".into() } else { task.paths.join("、") },
            if task.criteria.is_empty() { "—".into() } else { task.criteria.join("、") },
            if task.check.is_empty() { "相关测试".into() } else { task.check.join(" ") },
        ));
    }
    let optional: Vec<_> = stages
        .iter()
        .enumerate()
        .filter(|(i, _)| *i > req.stage && workflow::skippable(req.template, *i))
        .map(|(_, s)| s.name)
        .collect();
    if stage.role == Role::Plan && !optional.is_empty() {
        text.push_str(&format!(
            "\n后续可选阶段（{}）如本需求用不到（例如没有界面改动），单独一行写 AEP-SKIP: <阶段名>：<原因>。",
            optional.join("、")
        ));
    }
    let earlier: Vec<_> = stages[..req.stage.min(stages.len())]
        .iter()
        .map(|s| s.name)
        .collect();
    if !earlier.is_empty() {
        text.push_str(&format!(
            "\n问题必须由前序阶段（{}）先解决时（如需求不明、方案待定、实现缺陷），不要自行假设：最后单独一行写 AEP-BACK: <阶段名>：<需要对方解决的问题>。",
            earlier.join("、")
        ));
    }
    if matches!(stage.role, Role::Test | Role::Accept) {
        text.push_str("最后单独一行写 AEP: PASS 或 AEP: BLOCKED，缺必要证据时标为 BLOCKED。");
    }
    if !req.profile.check_command.is_empty() {
        text.push_str(&format!(
            "\n已允许的精确验收命令：{}",
            req.profile.check_command
        ));
    }
    Ok(text)
}
/// Fresh-session prompt: `header + memory_body + rules + READING`.
pub fn packet(
    req: &Request,
    project: &[Value],
    repo: &[Value],
    since: Option<i64>,
) -> Result<Packet> {
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
    let mut text = header(req)?;
    if let Some(transfer) = &req.stage_transfer {
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
    let body = memory_body(req, &text, &project, &repo);
    text.push_str(&body);
    text.push_str(&rules(req)?);
    text.push('\n');
    text.push_str(READING);
    Ok(Packet {
        more: false,
        text,
        project_ids,
        repo_ids,
    })
}
/// Prompt for a stage that reuses the previous stage's session (R3): the session already holds
/// memory and earlier outputs, so only `header + rules + READING` and this run's archive are sent.
pub fn adjacent(req: &Request, dir: &Path) -> Result<String> {
    Ok(format!(
        "{}{}\n{READING}\n本次档案 {}。前序执行的 result.txt 和 evidence.json 位于同级执行目录。",
        header(req)?,
        rules(req)?,
        dir.display()
    ))
}
/// New-session prompt used instead of resuming (R2, E4, E5): the full fresh `base` prompt plus
/// the previous answer (first 8000 chars) and a hand-off digest (its AEP-HANDOFF line and the
/// source evidence file). `follow` is the operator's message when continuing the conversation.
pub fn with_previous(base: &str, previous: &str, evidence: &Path, follow: Option<&str>) -> String {
    let mut text = format!(
        "{base}\n\n## 上一轮答复\n{}\n\n## 交接摘要\n{}\n回执：{}",
        previous.chars().take(8000).collect::<String>(),
        summary(previous),
        evidence.display()
    );
    if let Some(message) = follow {
        text.push_str(&format!("\n\n## 继续的要求\n{message}"));
    }
    text
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
            subtask: None,
            subtasks_done: vec![],
            sandbox: self.sandbox(stage),
            follow_up: None,
            token_limit: None,
            resume_limit: None,
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
            mut profile,
            brief,
            repository,
            external,
        } = assignment;
        // An unset model means the project's saved default for that CLI, not whatever the
        // CLI's own config happens to name (which may not be available to this account).
        if external.is_none()
            && profile.model.trim().is_empty()
            && matches!(
                profile.client,
                workflow::Client::Codex | workflow::Client::Claude
            )
            && let Some(preset) = self
                .preset(profile.client)
                .filter(|p| !p.model.trim().is_empty())
        {
            profile.model = preset.model.clone();
        }
        if external.is_none() {
            profile.validate()?;
        }
        let project = self.name.clone();
        let pid = self.id.clone();
        let resume_limit = crate::session_reuse::limit(self.resume_limit);
        let budget = self
            .work
            .iter()
            .find(|w| w.id == work_id)
            .filter(|_| external.is_none())
            .map(|w| crate::usage::budget(catalog, self, w, stage));
        if let Some(reason) = budget.as_ref().and_then(|b| b.exhausted()) {
            anyhow::bail!(reason);
        }
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
        let mut request = item.request(
            &project,
            stage,
            Assignment {
                profile: profile.clone(),
                repository: repository.clone(),
                brief: brief.clone(),
                external,
            },
        );
        request.token_limit = budget.and_then(|b| b.remaining());
        request.resume_limit = Some(resume_limit);
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
        // After a send-back the stage has no current attempt; its last session still holds context.
        let previous = item
            .current_attempt
            .get(&stage)
            .cloned()
            .or_else(|| {
                item.attempts
                    .iter()
                    .rev()
                    .find(|a| a.stage == stage)
                    .map(|a| a.id.clone())
            })
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
#[cfg(test)]
mod tests {
    use super::*;

    fn req(stage: usize) -> Request {
        serde_json::from_value(json!({
            "project": "P",
            "work": "W",
            "template": "Software",
            "stage": stage,
            "profile": {"client": "Claude", "executable": "claude", "model": ""},
            "repository": null,
            "brief": "做点事",
            "approved": [["产品需求", "已确认的需求结论 XYZ"]],
            "prior_runs": [[0, "run-prev"]],
            "handoffs": [{"from": "w0", "name": "事项甲", "stage": "产品需求", "note": "交接备注 HHH"}],
        }))
        .unwrap()
    }
    fn memory() -> Vec<Value> {
        vec![json!({"id": 7, "kind": "decision", "body": "项目记忆正文 MMM"})]
    }

    #[test]
    fn handoff_fresh_prompt_keeps_sections_and_reading() {
        let r = req(1);
        let p = packet(&r, &memory(), &[], None).unwrap();
        let h = header(&r).unwrap();
        assert!(p.text.starts_with(&h));
        assert!(p.text.contains("阶段："));
        for part in [
            "项目记忆",
            "MMM",
            "已确认的需求结论 XYZ",
            "交接备注 HHH",
            "前序执行 run-prev",
        ] {
            assert!(p.text.contains(part), "missing {part}");
        }
        assert!(p.text.contains(&rules(&r).unwrap()));
        assert!(
            p.text.ends_with(READING),
            "AC-10: reading convention in fresh prompt"
        );
        assert_eq!(p.project_ids, vec![7]);
    }

    #[test]
    fn handoff_adjacent_prompt_has_no_memory_or_prior_output() {
        let r = req(1);
        let text = adjacent(&r, Path::new("/tmp/runs/abc")).unwrap();
        assert!(text.lines().nth(1).unwrap().starts_with("阶段："));
        assert!(
            text.contains(READING),
            "AC-10: reading convention in reused prompt"
        );
        assert!(text.contains("本次档案 /tmp/runs/abc"));
        assert!(text.contains("AEP-HANDOFF"));
        for part in ["项目记忆", "MMM", "XYZ", "HHH", "前序执行 run-prev"] {
            assert!(!text.contains(part), "reused prompt leaks {part}");
        }
    }

    #[test]
    fn handoff_new_session_carries_previous_answer_and_digest() {
        let r = req(3);
        let base = packet(&r, &memory(), &[], None).unwrap().text;
        let previous = "做完了一部分\nAEP-HANDOFF: 结论 OK；证据 target/x；下一步 T3";
        let text = with_previous(&base, previous, Path::new("/runs/src/evidence.json"), None);
        assert!(
            text.contains(READING),
            "AC-10: reading convention in fallback prompt"
        );
        assert!(text.contains("## 上一轮答复\n做完了一部分"));
        assert!(text.contains("## 交接摘要\n结论 OK；证据 target/x；下一步 T3"));
        assert!(text.contains("/runs/src/evidence.json"));
        assert!(
            text.contains("用 AEP-HANDOFF: 单行"),
            "AC-5: format rule kept"
        );
        assert!(!text.contains("## 继续的要求"));
        let long = "字".repeat(9000);
        let cont = with_previous(&base, &long, Path::new("e.json"), Some("继续改"));
        assert!(cont.ends_with("## 继续的要求\n继续改"));
        assert!(!cont.contains(&"字".repeat(8001)));
    }

    #[test]
    fn handoff_gate_rules_keep_verdict_line() {
        let r = req(4);
        let rules = rules(&r).unwrap();
        assert!(rules.contains("AEP: PASS 或 AEP: BLOCKED"), "AC-5");
        assert!(
            adjacent(&r, Path::new("d"))
                .unwrap()
                .contains("AEP: PASS 或 AEP: BLOCKED")
        );
    }
}
