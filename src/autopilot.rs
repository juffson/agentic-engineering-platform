//! Run a work item's remaining stages unattended.
//!
//! Each step re-reads the catalog, so Studio and other sessions always see the same state.
//! A passing stage is confirmed and the next one starts. A test or acceptance stage that fails
//! on code goes back to the build stage a bounded number of times; anything else — an
//! environment block, a repeated failure, a failure outside the gates — stops and waits for a
//! person. Nothing is ever waived automatically.
use crate::{
    handoff::{self, Assignment, busy},
    project::{Library, Project},
    workflow::{self, Role},
};
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashSet,
    fs::{self, File},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

/// Automatic returns from one gate to the build stage before a person must decide.
pub const MAX_REWORKS: usize = 2;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct State {
    /// `running`, `done`, `stopped` or `failed`.
    pub status: String,
    /// Last stage index (inclusive) the run may execute.
    pub until: usize,
    pub stage: Option<usize>,
    pub message: String,
    pub updated_at: String,
}

pub fn work_dir(catalog: &Path, project: &str, work: &str) -> PathBuf {
    catalog.join("workflows").join(project).join(work)
}
fn lock_file(dir: &Path) -> Result<File> {
    fs::create_dir_all(dir)?;
    Ok(File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("autopilot.lock"))?)
}
pub fn read(dir: &Path) -> Option<State> {
    serde_json::from_slice(&fs::read(dir.join("autopilot.json")).ok()?).ok()
}
/// True while a runner process holds the item's autopilot lock.
pub fn running(dir: &Path) -> bool {
    dir.join("autopilot.lock").is_file() && lock_file(dir).is_ok_and(|f| f.try_lock().is_err())
}
/// Ask the runner to cancel the current agent and stop.
pub fn request_stop(dir: &Path) -> Result<()> {
    Ok(fs::write(dir.join("autopilot.stop"), b"stop")?)
}
fn stop_requested(dir: &Path) -> bool {
    dir.join("autopilot.stop").exists()
}
fn save(dir: &Path, state: &mut State) -> Result<()> {
    state.updated_at = chrono::Utc::now().to_rfc3339();
    crate::project::save_json(dir, "autopilot.json", state)
}

fn project<'a>(lib: &'a Library, id: &str) -> Result<&'a Project> {
    lib.projects
        .iter()
        .find(|p| p.id == id)
        .context("项目不存在")
}
fn project_mut<'a>(lib: &'a mut Library, id: &str) -> Result<&'a mut Project> {
    lib.projects
        .iter_mut()
        .find(|p| p.id == id)
        .context("项目不存在")
}

/// Run stages from the item's current stage through `until`. `exe` is the `aep` binary that
/// executes one stage (`aep workflow-stage --request …`).
pub fn run(catalog: &Path, pid: &str, wid: &str, until: usize, exe: &Path) -> Result<()> {
    let dir = work_dir(catalog, pid, wid);
    let lock = lock_file(&dir)?;
    lock.try_lock().map_err(|_| anyhow!("此事项已在自动运行"))?;
    let _ = fs::remove_file(dir.join("autopilot.stop"));
    let mut state = State {
        status: "running".into(),
        until,
        message: "自动运行已开始".into(),
        ..Default::default()
    };
    save(&dir, &mut state)?;
    let outcome = drive(catalog, pid, wid, until, exe, &dir, &mut state);
    let (status, message) = match outcome {
        Ok(end) => end,
        Err(e) => ("failed".to_owned(), format!("{e:#}")),
    };
    state.status = status;
    state.message = message;
    save(&dir, &mut state)?;
    let _ = fs::remove_file(dir.join("autopilot.stop"));
    Ok(())
}

fn drive(
    catalog: &Path,
    pid: &str,
    wid: &str,
    until: usize,
    exe: &Path,
    dir: &Path,
    state: &mut State,
) -> Result<(String, String)> {
    // Attempts this run created; a failure we did not cause gets one fresh try first.
    let mut ours: HashSet<String> = HashSet::new();
    // Stages whose CLI already crashed once in this run.
    let mut retried: HashSet<usize> = HashSet::new();
    let mut path_fixes: HashSet<usize> = HashSet::new();
    loop {
        if stop_requested(dir) {
            return Ok(("stopped".into(), "已手动停止自动运行".into()));
        }
        let mut lib = Library::open(catalog)?;
        let p = project(&lib, pid)?;
        let work = p.work.iter().find(|w| w.id == wid).context("事项不存在")?;
        let stages = work.template.stages();
        let i = work.stage;
        if i >= stages.len() {
            return Ok(("done".into(), "全部阶段已完成".into()));
        }
        if i > until {
            return Ok((
                "done".into(),
                format!(
                    "已自动运行到「{}」，停在「{}」等你处理",
                    stages[until].name, stages[i].name
                ),
            ));
        }
        let name = stages[i].name;
        state.stage = Some(i);
        if let Some(reason) = work
            .skipped
            .get(&i)
            .filter(|_| !work.current_attempt.contains_key(&i))
            .cloned()
        {
            state.message = format!("跳过「{name}」：{reason}");
            save(dir, state)?;
            lib.change(catalog, |lib| {
                project_mut(lib, pid)?.skip_stage(catalog, wid, i, &reason)
            })?;
            continue;
        }
        let current = work.current_attempt.get(&i).cloned();
        let ev = current
            .as_ref()
            .map(|id| workflow::evidence(&workflow::attempt_dir(catalog, pid, wid, id)))
            .transpose()?;
        // A planned build runs one subtask per session until every subtask has passed.
        if stages[i].role == Role::Build && ev.as_ref().is_none_or(|e| e["status"] == "succeeded") {
            let done = done_subtasks(catalog, pid, work, i);
            if let Some(task) = subtasks(catalog, pid, work)
                .into_iter()
                .find(|t| !done.contains(&t.id))
            {
                state.message = format!("「{name}」子任务 {} {}", task.id, task.title);
                save(dir, state)?;
                let attempt = lib.change(catalog, |lib| {
                    start(project_mut(lib, pid)?, catalog, wid, i)
                })?;
                let mut request: workflow::Request =
                    serde_json::from_slice(&fs::read(attempt.join("request.json"))?)?;
                request.subtask = Some(task);
                crate::project::save_json(&attempt, "request.json", &request)?;
                if !execute(exe, &attempt, dir, &mut ours)? {
                    return Ok(("stopped".into(), "已手动停止自动运行".into()));
                }
                continue;
            }
        }
        match ev
            .as_ref()
            .map(|e| e["status"].as_str().unwrap_or_default())
        {
            Some(_) if ev.as_ref().is_some_and(busy) => {
                state.message = format!("等待「{name}」当前执行完成");
                save(dir, state)?;
                let attempt = workflow::attempt_dir(catalog, pid, wid, current.as_deref().unwrap());
                if !wait_for(&attempt, dir, None)? {
                    return Ok(("stopped".into(), "已手动停止自动运行".into()));
                }
            }
            Some("succeeded") => {
                state.message = format!("「{name}」已通过，确认并进入下一阶段");
                save(dir, state)?;
                lib.change(catalog, |lib| {
                    project_mut(lib, pid)?.finish_stage(catalog, wid, i, String::new())
                })?;
            }
            Some(_)
                if current.as_ref().is_some_and(|id| ours.contains(id))
                    || ev.as_ref().is_some_and(|e| e["route_back"].is_object()) =>
            {
                let failed = workflow::attempt_dir(catalog, pid, wid, current.as_deref().unwrap());
                let e = ev.as_ref().unwrap();
                // The CLI itself broke (bad model, network, login): nothing to route back.
                // Retry once for transient trouble, then stop with the CLI's own reason.
                if e["route_back"].is_null() && e["exit_code"].as_i64().is_some_and(|c| c != 0) {
                    let error = e["error"].as_str().unwrap_or("执行失败");
                    let detail: String = error
                        .lines()
                        .skip(1)
                        .find(|l| !l.trim().is_empty())
                        .or(error.lines().next())
                        .unwrap_or_default()
                        .chars()
                        .take(240)
                        .collect();
                    if !retried.insert(i) {
                        return Ok((
                            "stopped".into(),
                            format!(
                                "「{name}」的 CLI 执行出错：{detail}。请检查此阶段的 CLI 和模型配置后再运行"
                            ),
                        ));
                    }
                    state.message = format!("「{name}」CLI 异常退出，自动重试一次：{detail}");
                    save(dir, state)?;
                    let attempt = lib.change(catalog, |lib| {
                        start(project_mut(lib, pid)?, catalog, wid, i)
                    })?;
                    if !execute(exe, &attempt, dir, &mut ours)? {
                        return Ok(("stopped".into(), "已手动停止自动运行".into()));
                    }
                    continue;
                }
                // The plan named files that do not exist: its own session fixes the plan, once.
                if e["invalid_paths"].as_array().is_some_and(|a| !a.is_empty())
                    && path_fixes.insert(i)
                {
                    let error = e["error"].as_str().unwrap_or("子任务路径无效");
                    state.message = format!("「{name}」的子任务路径有误，已请它修正");
                    save(dir, state)?;
                    let message = format!(
                        "{error}\n\n请在仓库里核对这些路径（ls / rg），修正 task.json 后照常给出本阶段结论。"
                    );
                    let attempt = lib.change(catalog, |lib| {
                        continue_or_start(project_mut(lib, pid)?, catalog, wid, i, &message)
                    })?;
                    if !execute(exe, &attempt, dir, &mut ours)? {
                        return Ok(("stopped".into(), "已手动停止自动运行".into()));
                    }
                    continue;
                }
                let report = fs::read_to_string(failed.join("result.txt")).unwrap_or_default();
                let blocked = report
                    .trim()
                    .lines()
                    .last()
                    .is_some_and(|l| l.contains("BLOCKED"));
                let gate = matches!(stages[i].role, Role::Test | Role::Accept);
                // The stage named who must act; otherwise a failing gate goes back to the build.
                let routed = e["route_back"]["stage"].as_u64().map(|s| s as usize);
                let target = routed.or_else(|| {
                    (gate && !blocked)
                        .then(|| (0..i).rev().find(|&b| stages[b].role == Role::Build))
                        .flatten()
                });
                let reason = e["error"]
                    .as_str()
                    .unwrap_or("执行失败")
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                let Some(b) = target.filter(|&b| b < i && work.reworks_between(i, b) < MAX_REWORKS)
                else {
                    let why = if blocked {
                        "Agent 报告 BLOCKED，多半是执行环境受限".to_owned()
                    } else if let Some(b) = target {
                        format!("已自动退回「{}」{MAX_REWORKS} 次仍未解决", stages[b].name)
                    } else {
                        reason
                    };
                    return Ok((
                        "stopped".into(),
                        format!("「{name}」未通过（{why}），需要你决定：退回、继续对话或人工放行"),
                    ));
                };
                let question = e["route_back"]["question"]
                    .as_str()
                    .filter(|q| !q.trim().is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| {
                        let s = handoff::summary(&report);
                        if s.trim().is_empty() {
                            format!("「{name}」未通过")
                        } else {
                            s
                        }
                    });
                let message = rework_message(name, &question, &failed);
                state.message = format!("「{name}」需要「{}」先解决，已自动退回", stages[b].name);
                save(dir, state)?;
                let attempt = lib.change(catalog, |lib| {
                    let p = project_mut(lib, pid)?;
                    p.work_by_id_mut(wid)?.send_back(b, &question)?;
                    continue_or_start(p, catalog, wid, b, &message)
                })?;
                if !execute(exe, &attempt, dir, &mut ours)? {
                    return Ok(("stopped".into(), "已手动停止自动运行".into()));
                }
            }
            _ => {
                state.message = format!("正在执行「{name}」");
                save(dir, state)?;
                // A stage re-entered after an upstream fix re-checks in its own session.
                let revisit = revisiting(work, i);
                let message = recheck_message(work, i);
                let attempt = lib.change(catalog, |lib| {
                    let p = project_mut(lib, pid)?;
                    if revisit {
                        continue_or_start(p, catalog, wid, i, &message)
                    } else {
                        start(p, catalog, wid, i)
                    }
                })?;
                if !execute(exe, &attempt, dir, &mut ours)? {
                    return Ok(("stopped".into(), "已手动停止自动运行".into()));
                }
            }
        }
    }
}

/// The build plan the latest planning stage wrote: `task.json` → `subtasks`.
pub fn subtasks(catalog: &Path, pid: &str, work: &crate::project::Work) -> Vec<workflow::Subtask> {
    let stages = work.template.stages();
    for (i, _) in stages
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, s)| s.role == Role::Plan)
    {
        let Some(ev) = work.current_attempt.get(&i).and_then(|id| {
            workflow::evidence(&workflow::attempt_dir(catalog, pid, &work.id, id)).ok()
        }) else {
            continue;
        };
        let Some(root) = ev["workspace"].as_str().map(PathBuf::from) else {
            continue;
        };
        for file in ev["spec_files"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !file.ends_with("task.json") {
                continue;
            }
            let plan: Value = fs::read(root.join(file))
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            let tasks: Vec<workflow::Subtask> =
                serde_json::from_value(plan["subtasks"].clone()).unwrap_or_default();
            if !tasks.is_empty() {
                return tasks;
            }
        }
    }
    vec![]
}

/// Subtask IDs whose build session passed.
pub fn done_subtasks(
    catalog: &Path,
    pid: &str,
    work: &crate::project::Work,
    stage: usize,
) -> HashSet<String> {
    work.attempts
        .iter()
        .filter(|a| a.stage == stage)
        .filter_map(|a| {
            let dir = workflow::attempt_dir(catalog, pid, &work.id, &a.id);
            (workflow::evidence(&dir).ok()?["status"] == "succeeded").then_some(())?;
            let req: workflow::Request =
                serde_json::from_slice(&fs::read(dir.join("request.json")).ok()?).ok()?;
            req.subtask.map(|t| t.id)
        })
        .collect()
}

/// A stage entered again after a later stage sent the item back: it has run before but its
/// conclusion was cleared, so it should re-check in its own session.
pub fn revisiting(work: &crate::project::Work, stage: usize) -> bool {
    !work.current_attempt.contains_key(&stage)
        && work.attempts.iter().any(|a| a.stage == stage)
        && work.reworks.iter().any(|r| r.to < stage)
}

/// What a re-entered stage is told: why the item went back and what the upstream fix says.
/// Resumed sessions only receive this message, so it carries the hand-off itself.
pub fn recheck_message(work: &crate::project::Work, stage: usize) -> String {
    let stages = work.template.stages();
    let mut text = String::from(
        "前序阶段已按反馈修改。请在事项分支当前的最新提交上复核你上次指出的问题，确认是否已解决、有没有引入新问题，再照常给出本阶段结论。",
    );
    if let Some(r) = work.reworks.iter().rev().find(|r| r.to < stage) {
        text.push_str(&format!(
            "\n\n最近一次退回：「{}」→「{}」\n原因：{}",
            stages.get(r.from).map_or("", |s| s.name),
            stages.get(r.to).map_or("", |s| s.name),
            handoff::clip(r.reason.trim(), 1500)
        ));
    }
    if let Some(t) = work.incoming(stage) {
        text.push_str(&format!(
            "\n\n「{}」修改后的交接：\n{}",
            stages.get(t.from).map_or("", |s| s.name),
            handoff::clip(t.decision.trim(), 2000)
        ));
    }
    text
}

/// What the stage that must fix a problem is told: the request, and the reviewing stage's
/// full report inline (the file may be outside the fixing agent's sandbox), plus its path.
pub fn rework_message(from: &str, request: &str, failed_attempt: &Path) -> String {
    let path = failed_attempt.join("result.txt");
    let report = fs::read_to_string(&path).unwrap_or_default();
    let mut text = format!("「{from}」未通过，需要你先解决：\n{}\n", request.trim());
    if !report.trim().is_empty() {
        text.push_str(&format!(
            "\n「{from}」的完整报告：\n<<<\n{}\n>>>\n",
            handoff::clip(report.trim(), 6000)
        ));
    }
    text.push_str(&format!(
        "\n报告文件：{}\n修复后照常给出本阶段结论；回到「{from}」时它会按你的结论复核。",
        path.display()
    ));
    text
}

/// A fresh run of the stage with its saved brief and CLI.
fn start(p: &mut Project, catalog: &Path, wid: &str, stage: usize) -> Result<PathBuf> {
    let w = p.work_by_id_mut(wid)?;
    let assignment = Assignment {
        profile: w.profile(stage),
        brief: w.notes[stage].clone(),
        repository: w.target(stage),
        external: None,
    };
    p.reserve(catalog, wid, stage, assignment)
}

/// Continue the stage's last session when it can be continued, otherwise start a fresh run
/// whose brief carries the message.
pub fn continue_or_start(
    p: &mut Project,
    catalog: &Path,
    wid: &str,
    stage: usize,
    message: &str,
) -> Result<PathBuf> {
    let work = p.work_by_id_mut(wid)?;
    let last = work.current_attempt.get(&stage).cloned().or_else(|| {
        work.attempts
            .iter()
            .rev()
            .find(|a| a.stage == stage)
            .map(|a| a.id.clone())
    });
    let continuable = last
        .and_then(|id| workflow::evidence(&workflow::attempt_dir(catalog, &p.id, wid, &id)).ok())
        .is_some_and(|e| e["mode"] != "external" && e["workspace"].is_string());
    if continuable {
        return p.reserve_follow_up(catalog, wid, stage, message);
    }
    let work = p.work_by_id_mut(wid)?;
    let assignment = Assignment {
        profile: work.profile(stage),
        brief: message.to_owned(),
        repository: work.target(stage),
        external: None,
    };
    p.reserve(catalog, wid, stage, assignment)
}

/// Run one reserved attempt to completion. Returns false when the user stopped the run.
fn execute(exe: &Path, attempt: &Path, dir: &Path, ours: &mut HashSet<String>) -> Result<bool> {
    let id = attempt
        .file_name()
        .and_then(|s| s.to_str())
        .context("invalid attempt")?;
    ours.insert(id.to_owned());
    let log = File::create(attempt.join("supervisor.log"))?;
    let child = Command::new(exe)
        .arg("workflow-stage")
        .arg("--request")
        .arg(attempt.join("request.json"))
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .spawn()
        .context("无法启动阶段执行")?;
    wait_for(attempt, dir, Some(child))
}

/// Wait until the attempt is no longer busy, cancelling it if a stop is requested.
fn wait_for(attempt: &Path, dir: &Path, mut child: Option<std::process::Child>) -> Result<bool> {
    let mut stopping = false;
    loop {
        if let Some(c) = child.as_mut()
            && c.try_wait()?.is_some()
        {
            child = None;
        }
        let ev: Value = workflow::evidence(attempt).unwrap_or_default();
        let done = child.is_none() && !busy(&ev);
        if done {
            return Ok(!stopping);
        }
        if !stopping && stop_requested(dir) {
            stopping = true;
            let _ = handoff::cancel(attempt);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}
