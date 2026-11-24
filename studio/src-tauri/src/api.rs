//! Studio commands as plain functions over the catalog directory. Shapes follow
//! `studio/src/api/types.ts`.
use crate::runner;
use aep::{
    autopilot,
    handoff::{self, Assignment, busy},
    memory::{Key, Memory, TEXT_LIMIT},
    project::{Kind, Library, Project, Work, import, merge, shelf},
    transcript::{self, Entry},
    workflow::{self, Client, Profile, Request, Role, Sandbox, Template},
    workspace::Workspace,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

fn template_id(t: Template) -> &'static str {
    match t {
        Template::Software => "Software",
        Template::Game => "Game",
        Template::Legacy => "Legacy",
    }
}
fn role_id(r: Role) -> &'static str {
    match r {
        Role::Plan => "Plan",
        Role::Build => "Build",
        Role::Test => "Test",
        Role::Accept => "Accept",
        Role::Deliver => "Deliver",
    }
}
fn kind_id(k: Kind) -> &'static str {
    match k {
        Kind::Feature => "Feature",
        Kind::Release => "Release",
    }
}
fn find<'a>(lib: &'a Library, pid: &str) -> Result<&'a Project> {
    lib.projects
        .iter()
        .find(|p| p.id == pid)
        .context("项目不存在")
}
fn find_mut<'a>(lib: &'a mut Library, pid: &str) -> Result<&'a mut Project> {
    lib.projects
        .iter_mut()
        .find(|p| p.id == pid)
        .context("项目不存在")
}
fn item<'a>(p: &'a Project, wid: &str) -> Result<&'a Work> {
    p.work.iter().find(|w| w.id == wid).context("事项不存在")
}
/// Read an attempt's evidence; the runner rewrites the file, so retry a torn read briefly.
fn read_evidence(dir: &Path) -> Value {
    for _ in 0..5 {
        if let Ok(v) = workflow::evidence(dir) {
            return v;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Value::Null
}
fn evidence(home: &Path, pid: &str, wid: &str, attempt: &str) -> Value {
    read_evidence(&workflow::attempt_dir(home, pid, wid, attempt))
}
/// Busy, or just reserved and its runner has not taken the lock yet.
fn active(home: &Path, pid: &str, wid: &str, attempt: &str) -> bool {
    let dir = workflow::attempt_dir(home, pid, wid, attempt);
    let ev = read_evidence(&dir);
    busy(&ev)
        || (ev["status"] == "unstarted"
            && fs::metadata(dir.join("request.json"))
                .and_then(|m| m.modified())
                .is_ok_and(|t| t.elapsed().is_ok_and(|e| e < Duration::from_secs(30))))
}
fn running_attempt(home: &Path, pid: &str, w: &Work) -> Option<String> {
    w.attempts
        .last()
        .filter(|a| active(home, pid, &w.id, &a.id))
        .map(|a| a.id.clone())
}
fn autopilot_dir(home: &Path, pid: &str, wid: &str) -> PathBuf {
    autopilot::work_dir(home, pid, wid)
}
/// Apply `edit` to a fresh copy of the catalog; concurrent writers are retried once.
fn change<T>(home: &Path, mut edit: impl FnMut(&mut Library) -> Result<T>) -> Result<T> {
    let mut lib = Library::open(home)?;
    match lib.change(home, &mut edit) {
        Ok(v) => Ok(v),
        Err(_) if lib.revision != Library::open(home)?.revision => {
            let mut lib = Library::open(home)?;
            lib.change(home, &mut edit)
        }
        Err(e) => Err(e),
    }
}
fn ensure_idle(home: &Path, pid: &str, w: &Work) -> Result<()> {
    ensure!(
        running_attempt(home, pid, w).is_none(),
        "此事项已有 Agent 正在运行，请等待完成或停止"
    );
    ensure!(
        !autopilot::running(&autopilot_dir(home, pid, &w.id)),
        "此事项正在自动运行，请先停止自动运行"
    );
    Ok(())
}

pub fn snapshot(home: &Path) -> Result<Value> {
    let lib = Library::open(home)?;
    let projects: Vec<Value> = lib
        .projects
        .iter()
        .map(|p| {
            let work: Vec<Value> = p
                .work
                .iter()
                .rev()
                .map(|w| {
                    let stages = w.template.stages();
                    let status = w
                        .current_attempt
                        .get(&w.stage)
                        .map(|a| evidence(home, &p.id, &w.id, a)["status"].clone())
                        .unwrap_or(json!(""));
                    let group = match (w.delivered(), w.shelved.is_some()) {
                        (false, _) => "active",
                        (true, false) => "delivered",
                        (true, true) => "shelved",
                    };
                    json!({
                        "id": w.id, "name": w.name, "kind": kind_id(w.kind), "group": group,
                        "stage": w.stage, "stage_count": stages.len(), "stage_name": w.stage_name(),
                        "status": status,
                        "running": running_attempt(home, &p.id, w).is_some(),
                        "autopilot": autopilot::running(&autopilot_dir(home, &p.id, &w.id)),
                    })
                })
                .collect();
            json!({"id": p.id, "name": p.name, "repositories": p.repositories, "work": work})
        })
        .collect();
    let clients: Vec<Value> = Client::ALL
        .iter()
        .map(|c| json!({"id": format!("{c:?}"), "label": c.label(), "models": workflow::suggested_models(*c)}))
        .collect();
    let sandboxes: Vec<Value> = Sandbox::ALL
        .iter()
        .map(|s| json!({"id": format!("{s:?}"), "label": s.label()}))
        .collect();
    let templates: Vec<Value> = [Template::Software, Template::Game]
        .iter()
        .map(|t| json!({"id": template_id(*t), "label": t.label(), "stages": t.stages().iter().map(|s| s.name).collect::<Vec<_>>()}))
        .collect();
    Ok(json!({
        "home": home,
        "active": lib.active,
        "selected": lib.project().and_then(|p| p.selected.clone()),
        "projects": projects,
        "clients": clients,
        "sandboxes": sandboxes,
        "templates": templates,
    }))
}

pub fn open_project(home: &Path, pid: &str) -> Result<()> {
    change(home, |lib| {
        find(lib, pid)?;
        lib.active = Some(pid.into());
        Ok(())
    })
}

/// Take a project out of Studio. Repositories are untouched; Studio's own records for it
/// move to `workflows/.removed/` so they can be restored by hand.
pub fn remove_project(home: &Path, pid: &str) -> Result<()> {
    let lib = Library::open(home)?;
    let p = find(&lib, pid)?;
    for w in &p.work {
        ensure!(
            running_attempt(home, pid, w).is_none()
                && !autopilot::running(&autopilot::work_dir(home, pid, &w.id)),
            "「{}」还有 Agent 在运行，先停止再移除项目",
            w.name
        );
    }
    change(home, |lib| {
        find(lib, pid)?;
        lib.projects.retain(|p| p.id != pid);
        if lib.active.as_deref() == Some(pid) {
            lib.active = lib.projects.first().map(|p| p.id.clone());
        }
        Ok(())
    })?;
    let dir = home.join("workflows").join(pid);
    if dir.is_dir() {
        let removed = home.join("workflows").join(".removed");
        fs::create_dir_all(&removed)?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        fs::rename(&dir, removed.join(format!("{pid}-{stamp}")))?;
    }
    Ok(())
}

pub fn select_work(home: &Path, pid: &str, wid: &str) -> Result<()> {
    change(home, |lib| {
        let p = find_mut(lib, pid)?;
        item(p, wid)?;
        p.selected = Some(wid.into());
        lib.active = Some(pid.into());
        Ok(())
    })
}

fn repository(path: &str) -> Result<PathBuf> {
    let ws = Workspace::open(Path::new(path.trim())).context("不是 Git 仓库")?;
    Ok(ws.root.canonicalize()?)
}

/// Create a project and return its ID.
pub fn create_project(home: &Path, name: &str, repositories: &[String]) -> Result<String> {
    let repos = repositories
        .iter()
        .map(|r| repository(r))
        .collect::<Result<Vec<_>>>()?;
    change(home, |lib| {
        lib.create(name, repos.clone())?;
        lib.projects
            .iter()
            .find(|p| p.name == name.trim())
            .map(|p| p.id.clone())
            .context("项目创建失败")
    })
}

/// What Claude Code, Codex and ZCode already hold for a repository folder.
pub fn import_scan(path: &str) -> Result<import::Found> {
    import::scan(&repository(path)?)
}

/// Import chosen Claude memory files and existing branches into a project. Returns how many
/// notes and items were created.
pub fn import_apply(
    home: &Path,
    pid: &str,
    path: &str,
    memories: &[String],
    branches: &[(String, String)],
) -> Result<Value> {
    let repo = repository(path)?;
    let lib = Library::open(home)?;
    let notes = find(&lib, pid)?.import_memories(home, &repo, memories)?;
    let items = change(home, |lib| {
        let p = find_mut(lib, pid)?;
        branches
            .iter()
            .map(|(branch, from)| p.import_branch(&repo, branch, Path::new(from)))
            .collect::<Result<Vec<_>>>()
    })?;
    Ok(json!({"notes": notes.len(), "items": items}))
}

/// Ask for a folder with the system folder chooser. A folder inside a git repository
/// resolves to the repository root; `None` when the user cancels.
pub fn pick_folder(prompt: &str) -> Result<Option<String>> {
    let script = format!(
        "POSIX path of (choose folder with prompt {})",
        serde_json::to_string(prompt)?
    );
    let out = std::process::Command::new("/usr/bin/osascript")
        .args(["-e", &script])
        .output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        // -128: the user pressed Cancel.
        ensure!(err.contains("-128"), "无法打开文件夹选择：{}", err.trim());
        return Ok(None);
    }
    let picked = String::from_utf8_lossy(&out.stdout)
        .trim()
        .trim_end_matches('/')
        .to_owned();
    let root = aep::workspace::git(Path::new(&picked), &["rev-parse", "--show-toplevel"])
        .map(|r| r.trim().to_owned())
        .unwrap_or(picked);
    Ok(Some(root))
}

pub fn add_repository(home: &Path, pid: &str, path: &str) -> Result<()> {
    let repo = repository(path)?;
    change(home, |lib| {
        let p = find_mut(lib, pid)?;
        ensure!(!p.repositories.contains(&repo), "仓库已关联");
        p.repositories.push(repo.clone());
        Ok(())
    })
}

pub fn create_work(
    home: &Path,
    pid: &str,
    name: &str,
    template: &str,
    release: bool,
    repositories: &[String],
) -> Result<String> {
    let template = match template {
        "Game" => Template::Game,
        _ => Template::Software,
    };
    let kind = if release {
        Kind::Release
    } else {
        Kind::Feature
    };
    change(home, |lib| {
        lib.active = Some(pid.into());
        let p = find_mut(lib, pid)?;
        p.add_work(name, kind, template)?;
        let known = p.repositories.clone();
        let w = p.selected_mut()?;
        for r in repositories {
            let r = PathBuf::from(r);
            ensure!(known.contains(&r), "仓库未关联到项目：{}", r.display());
            w.include(&r);
        }
        Ok(w.id.clone())
    })
}

fn attempt_summary(home: &Path, pid: &str, wid: &str, id: &str) -> Value {
    let dir = workflow::attempt_dir(home, pid, wid, id);
    let ev = read_evidence(&dir);
    let req: Option<Request> = fs::read(dir.join("request.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    let (prompt, follow_up, subtask, client, model) = match &req {
        Some(r) => (
            r.follow_up
                .as_ref()
                .map_or(r.brief.clone(), |f| f.message.clone()),
            r.follow_up.is_some(),
            r.subtask
                .as_ref()
                .map(|t| format!("{} {}", t.id, t.title).trim().to_owned()),
            format!("{:?}", r.profile.client),
            r.profile.model.clone(),
        ),
        None => (String::new(), false, None, "Custom".into(), String::new()),
    };
    json!({
        "id": id,
        "status": ev["status"].as_str().unwrap_or("initializing"),
        "client": client,
        "model": model,
        "started_at": ev["started_at"],
        "finished_at": ev["finished_at"],
        "prompt": prompt,
        "follow_up": follow_up,
        "subtask": subtask,
        "error": ev["error"],
        "route_back": if ev["route_back"].is_object() { ev["route_back"].clone() } else { Value::Null },
        "committed": ev["committed"] == true,
        "head_sha": ev["head_sha"],
    })
}

pub fn work_detail(home: &Path, pid: &str, wid: &str) -> Result<Value> {
    let lib = Library::open(home)?;
    let p = find(&lib, pid)?;
    let w = item(p, wid)?;
    // Check liveness before reading results: a run that ends in between then shows as still
    // running for one poll, never as stopped with a stale status.
    let dir = autopilot_dir(home, pid, wid);
    let autopilot_running = autopilot::running(&dir);
    let running = running_attempt(home, pid, w);
    let stages: Vec<Value> = w
        .template
        .stages()
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let state = if w.skipped.contains_key(&i) && i < w.stage {
                "skipped"
            } else if i < w.stage {
                "done"
            } else if i == w.stage {
                "current"
            } else {
                "upcoming"
            };
            let attempts: Vec<Value> = w
                .attempts
                .iter()
                .filter(|a| a.stage == i)
                .map(|a| attempt_summary(home, pid, wid, &a.id))
                .collect();
            json!({
                "index": i, "name": s.name, "role": role_id(s.role), "brief": s.brief,
                "state": state, "skippable": w.skippable(i),
                "skipped_reason": w.skipped.get(&i),
                "note": w.notes.get(i).cloned().unwrap_or_default(),
                "profile": w.profile(i),
                "sandbox": format!("{:?}", w.sandbox(i)),
                "attempts": attempts,
                "current_attempt": w.current_attempt.get(&i),
            })
        })
        .collect();
    let state = autopilot::read(&dir);
    let trees = w.worktrees();
    let branches = w.branches();
    let branch = trees
        .first()
        .and_then(|t| {
            aep::workspace::git(t, &["branch", "--show-current"])
                .ok()
                .map(|b| b.trim().to_owned())
                .filter(|b| !b.is_empty())
        })
        .or_else(|| branches.first().map(|b| b.name.clone()));
    let deliveries: Vec<Value> = w
        .deliveries
        .iter()
        .map(|d| {
            let merged = d
                .merge
                .as_ref()
                .filter(|r| r.status == merge::MergeStatus::Merged)
                .map(|r| json!({"target": r.target_branch, "sha": r.merged_sha.clone().unwrap_or(r.candidate_sha.clone()), "at": r.merged_at}));
            json!({"run": d.run, "archived_at": d.archived_at, "merged": merged})
        })
        .collect();
    let draft = (w.delivered() && w.shelved.is_none())
        .then(|| w.memory_draft(home, pid).ok())
        .flatten();
    Ok(json!({
        "project": pid, "id": w.id, "name": w.name, "kind": kind_id(w.kind),
        "template": template_id(w.template), "template_label": w.template.label(),
        "stage": w.stage, "delivered": w.delivered(), "shelved": w.shelved,
        "repositories": w.repositories, "branch": branch, "worktrees": trees,
        "branches": branches.iter().map(|b| json!({"name": b.name, "merged": b.merged})).collect::<Vec<_>>(),
        "stages": stages,
        "reworks": w.reworks.iter().map(|r| json!({"from": r.from, "to": r.to, "reason": r.reason, "at": r.at})).collect::<Vec<_>>(),
        "running": running,
        "autopilot": {
            "running": autopilot_running,
            "status": state.as_ref().map(|s| s.status.clone()),
            "message": state.as_ref().map(|s| s.message.clone()),
            "stage": state.as_ref().and_then(|s| s.stage),
            "until": state.as_ref().map(|s| s.until),
        },
        "deliveries": deliveries,
        "memory_draft": draft,
    }))
}

fn entry(e: Entry) -> Value {
    match e {
        Entry::Message(text) => json!({"kind":"message","text":text}),
        Entry::Thinking(text) => json!({"kind":"thinking","text":text}),
        Entry::Command {
            command,
            output,
            exit,
        } => json!({"kind":"command","command":command,"output":output,"exit":exit}),
        Entry::Edit(files) => json!({"kind":"edit","files":files}),
        Entry::Tool {
            name,
            detail,
            output,
            failed,
        } => json!({"kind":"tool","name":name,"detail":detail,"output":output,"failed":failed}),
        Entry::Error(text) => json!({"kind":"error","text":text}),
        Entry::Notice(text) => json!({"kind":"notice","text":text}),
        Entry::Text(text) => json!({"kind":"text","text":text}),
    }
}

pub fn attempt_view(home: &Path, pid: &str, wid: &str, attempt: &str) -> Result<Value> {
    aep::workspace::ident(attempt, 64)?;
    let dir = workflow::attempt_dir(home, pid, wid, attempt);
    ensure!(dir.is_dir(), "执行记录不存在");
    let ev = read_evidence(&dir);
    let raw = fs::read_to_string(dir.join("stdout.log")).unwrap_or_default();
    let t = transcript::parse(&raw);
    Ok(json!({
        "id": attempt,
        "status": ev["status"].as_str().unwrap_or("initializing"),
        "entries": t.entries.into_iter().map(entry).collect::<Vec<_>>(),
        "input_tokens": t.input_tokens,
        "output_tokens": t.output_tokens,
        "model": t.model,
        "error": ev["error"],
        "result": fs::read_to_string(dir.join("result.txt")).ok(),
        "zcode": zcode_task(&dir),
        "dir": dir,
        "workspace": ev["workspace"],
    }))
}

/// The ZCode desktop task behind an attempt (in the attempt folder or a role sub-folder):
/// whether it is waiting for a tool approval right now, and whether it can be opened.
fn zcode_task(dir: &Path) -> Option<Value> {
    let sub = [
        dir.to_path_buf(),
        dir.join("zcode-edit"),
        dir.join("zcode-plan"),
    ]
    .into_iter()
    .find(|d| {
        d.join("desktop-create.json").is_file() || d.join("desktop-approvals.json").is_file()
    })?;
    let path = sub.join("desktop-approvals.json");
    let observed = aep::workspace::read(&path).unwrap_or_default();
    // The monitor rewrites the file every two seconds while the task runs.
    let fresh = path
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < Duration::from_secs(15));
    let state = &observed["state"];
    let tools: Vec<Value> = state["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| json!({"name": t["tool"].as_str().unwrap_or("工具"), "detail": t["detail"].as_str().unwrap_or("")}))
        .collect();
    Some(json!({
        "waiting": fresh && state["waiting"] == true,
        "tools": tools,
        "session": observed["session"].as_str(),
        "openable": sub.join("desktop-create.json").is_file(),
    }))
}

/// Bring up the exact ZCode desktop task an attempt dispatched, e.g. to approve a tool.
pub fn zcode_open(home: &Path, pid: &str, wid: &str, attempt: &str) -> Result<()> {
    let dir = workflow::attempt_dir(home, pid, wid, attempt);
    let sub = [dir.clone(), dir.join("zcode-edit"), dir.join("zcode-plan")]
        .into_iter()
        .find(|d| d.join("desktop-create.json").is_file())
        .context("这次执行没有可打开的 ZCode 桌面任务")?;
    let out = std::process::Command::new(runner::binary()?)
        .env("PATH", runner::path()?)
        .arg("zcode-open")
        .arg("--dir")
        .arg(&sub)
        .output()?;
    ensure!(
        out.status.success(),
        "无法打开 ZCode 任务：{}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(())
}

fn parse_sandbox(s: &str) -> Result<Sandbox> {
    Sandbox::ALL
        .into_iter()
        .find(|x| format!("{x:?}") == s)
        .context("未知的权限选项")
}

pub fn update_stage(
    home: &Path,
    pid: &str,
    wid: &str,
    stage: usize,
    note: Option<String>,
    profile: Option<Profile>,
    sandbox: Option<String>,
) -> Result<()> {
    let sandbox = sandbox.as_deref().map(parse_sandbox).transpose()?;
    change(home, |lib| {
        let p = find_mut(lib, pid)?;
        let w = p.work_by_id_mut(wid)?;
        ensure!(stage < w.template.stages().len(), "阶段不存在");
        if let Some(n) = &note {
            w.notes[stage] = n.clone();
        }
        if let Some(pr) = &profile {
            w.profiles.insert(stage, pr.clone());
        }
        if let Some(s) = sandbox {
            w.sandboxes.insert(stage, s);
        }
        Ok(())
    })
}

/// Use this profile for the stage, and make it the project's default for its CLI.
fn remember(p: &mut Project, wid: &str, stage: usize, profile: &Profile) -> Result<()> {
    p.cli_defaults.retain(|o| o.client != profile.client);
    let mut preset = profile.clone();
    preset.check_command.clear();
    p.cli_defaults.push(preset);
    p.work_by_id_mut(wid)?
        .profiles
        .insert(stage, profile.clone());
    Ok(())
}

fn attempt_id(dir: &Path) -> String {
    dir.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub fn run_stage(
    home: &Path,
    pid: &str,
    wid: &str,
    stage: usize,
    message: Option<String>,
) -> Result<String> {
    let lib = Library::open(home)?;
    let w = item(find(&lib, pid)?, wid)?;
    ensure!(stage == w.stage, "只能运行当前阶段");
    ensure_idle(home, pid, w)?;
    let profile = w.profile(stage);
    profile.validate()?;
    runner::binary()?;
    // Re-entered after a send-back: re-check in the stage's own session, like autopilot.
    let recheck = autopilot::revisiting(w, stage).then(|| {
        let mut m = autopilot::recheck_message(w, stage);
        if let Some(extra) = message.as_deref().map(str::trim).filter(|x| !x.is_empty()) {
            m.push_str(&format!("\n\n补充说明：{extra}"));
        }
        m
    });
    let dir = change(home, |lib| {
        let p = find_mut(lib, pid)?;
        remember(p, wid, stage, &profile)?;
        if let Some(m) = &recheck {
            return autopilot::continue_or_start(p, home, wid, stage, m);
        }
        let w = p.work_by_id_mut(wid)?;
        let assignment = Assignment {
            profile: profile.clone(),
            brief: w.notes[stage].clone(),
            repository: w.target(stage),
            external: None,
        };
        p.reserve(home, wid, stage, assignment)
    })?;
    runner::stage(&dir)?;
    Ok(attempt_id(&dir))
}

pub fn follow_up(home: &Path, pid: &str, wid: &str, stage: usize, message: &str) -> Result<String> {
    ensure!(!message.trim().is_empty(), "请输入要继续对话的内容");
    let lib = Library::open(home)?;
    ensure_idle(home, pid, item(find(&lib, pid)?, wid)?)?;
    runner::binary()?;
    let dir = change(home, |lib| {
        find_mut(lib, pid)?.reserve_follow_up(home, wid, stage, message)
    })?;
    runner::stage(&dir)?;
    Ok(attempt_id(&dir))
}

pub fn cancel(home: &Path, pid: &str, wid: &str, attempt: &str) -> Result<()> {
    aep::workspace::ident(attempt, 64)?;
    handoff::cancel(&workflow::attempt_dir(home, pid, wid, attempt))
}

pub fn advance(home: &Path, pid: &str, wid: &str, stage: usize) -> Result<()> {
    let lib = Library::open(home)?;
    ensure_idle(home, pid, item(find(&lib, pid)?, wid)?)?;
    change(home, |lib| {
        let p = find_mut(lib, pid)?;
        let note = item(p, wid)?.notes.get(stage).cloned().unwrap_or_default();
        p.finish_stage(home, wid, stage, note)
    })
}

/// Send the item back to `to` and start the fix there: continue its last session when it
/// can be continued, otherwise run it afresh with the failure as its brief.
pub fn send_back(
    home: &Path,
    pid: &str,
    wid: &str,
    from: usize,
    to: usize,
    reason: Option<String>,
) -> Result<String> {
    let lib = Library::open(home)?;
    let w = item(find(&lib, pid)?, wid)?;
    ensure_idle(home, pid, w)?;
    ensure!(
        to < from && from == w.stage,
        "只能从当前阶段退回到更早的阶段"
    );
    let stages = w.template.stages();
    let report = w
        .current_attempt
        .get(&from)
        .map(|id| workflow::attempt_dir(home, pid, wid, id));
    let text = report
        .as_ref()
        .and_then(|d| fs::read_to_string(d.join("result.txt")).ok())
        .unwrap_or_default();
    let question = report
        .as_ref()
        .and_then(|d| workflow::evidence(d).ok())
        .and_then(|e| e["route_back"]["question"].as_str().map(str::to_owned));
    let reason = reason
        .filter(|r| !r.trim().is_empty())
        .or(question.filter(|q| !q.trim().is_empty()))
        .unwrap_or_else(|| handoff::summary(&text));
    let reason = if reason.trim().is_empty() {
        format!("「{}」未通过", stages[from].name)
    } else {
        reason
    };
    let message = match &report {
        Some(d) => autopilot::rework_message(stages[from].name, &reason, d),
        None => format!("「{}」未通过，需要你先解决：\n{reason}", stages[from].name),
    };
    runner::binary()?;
    let dir = change(home, |lib| {
        let p = find_mut(lib, pid)?;
        p.work_by_id_mut(wid)?.send_back(to, &reason)?;
        autopilot::continue_or_start(p, home, wid, to, &message)
    })?;
    runner::stage(&dir)?;
    Ok(attempt_id(&dir))
}

pub fn waive(home: &Path, pid: &str, wid: &str, stage: usize, reason: &str) -> Result<()> {
    change(home, |lib| {
        find_mut(lib, pid)?.waive_stage(home, wid, stage, reason)
    })
}

pub fn skip(home: &Path, pid: &str, wid: &str, stage: usize, reason: &str) -> Result<()> {
    change(home, |lib| {
        let p = find_mut(lib, pid)?;
        if stage == item(p, wid)?.stage {
            p.skip_stage(home, wid, stage, reason)
        } else {
            let w = p.work_by_id_mut(wid)?;
            // A later stage: mark it so it is passed over when reached; toggles off again.
            ensure!(
                w.skippable(stage) && stage > w.stage,
                "只有可选的规划阶段可以跳过"
            );
            if w.skipped.remove(&stage).is_none() {
                let reason = if reason.trim().is_empty() {
                    "手动选择跳过"
                } else {
                    reason.trim()
                };
                w.skipped.insert(stage, reason.into());
            }
            Ok(())
        }
    })
}

pub fn reopen(home: &Path, pid: &str, wid: &str, stage: usize) -> Result<()> {
    let lib = Library::open(home)?;
    ensure_idle(home, pid, item(find(&lib, pid)?, wid)?)?;
    change(home, |lib| {
        let w = find_mut(lib, pid)?.work_by_id_mut(wid)?;
        ensure!(stage < w.stage, "只能重新打开已完成的阶段");
        w.shelved = None;
        w.reopen_transfers(stage);
        w.stage = stage;
        w.current_attempt.retain(|s, _| *s < stage);
        Ok(())
    })
}

pub fn autopilot_start(home: &Path, pid: &str, wid: &str, until: usize) -> Result<()> {
    let lib = Library::open(home)?;
    let w = item(find(&lib, pid)?, wid)?;
    ensure_idle(home, pid, w)?;
    let stages = w.template.stages();
    ensure!(
        until >= w.stage && until < stages.len(),
        "请选择当前阶段之后的停止位置"
    );
    for (i, stage) in stages.iter().enumerate().take(until + 1).skip(w.stage) {
        if !w.skipped.contains_key(&i) {
            w.profile(i)
                .validate()
                .with_context(|| format!("「{}」的 CLI 配置无效", stage.name))?;
        }
    }
    let dir = autopilot_dir(home, pid, wid);
    let _ = fs::remove_file(dir.join("autopilot.stop"));
    runner::autopilot(home, pid, wid, until, &dir)?;
    // Give the runner a moment to take its lock so the UI shows it running.
    let deadline = Instant::now() + Duration::from_secs(3);
    while !autopilot::running(&dir) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

pub fn autopilot_stop(home: &Path, pid: &str, wid: &str) -> Result<()> {
    autopilot::request_stop(&autopilot_dir(home, pid, wid))
}

pub fn archive(
    home: &Path,
    pid: &str,
    wid: &str,
    memory: &str,
    remove_worktree: bool,
) -> Result<()> {
    let lib = Library::open(home)?;
    let w = item(find(&lib, pid)?, wid)?;
    ensure!(w.delivered(), "只有已交付的事项可以归档");
    ensure!(
        memory.trim().chars().count() <= TEXT_LIMIT,
        "记忆最多 {TEXT_LIMIT} 字"
    );
    if remove_worktree {
        // A dirty worktree stops the archive before anything is written; unmerged
        // branches are kept.
        w.clean(false)?;
    }
    change(home, |lib| {
        find_mut(lib, pid)?
            .archive_work(home, wid, memory)
            .map(|_| ())
    })
}

/// Remove a delivered item's worktrees and merged branches; `force` also deletes
/// unmerged branches.
pub fn cleanup(home: &Path, pid: &str, wid: &str, force: bool) -> Result<shelf::Cleaned> {
    let lib = Library::open(home)?;
    let w = item(find(&lib, pid)?, wid)?;
    ensure!(w.delivered(), "只有已交付的事项可以清理工作区和分支");
    w.clean(force)
}

/// Delete an item that never ran.
pub fn delete_work(home: &Path, pid: &str, wid: &str) -> Result<()> {
    change(home, |lib| find_mut(lib, pid)?.remove_work(home, wid))
}

pub fn unarchive(home: &Path, pid: &str, wid: &str) -> Result<()> {
    change(home, |lib| find_mut(lib, pid)?.unarchive_work(wid))
}

pub fn merge_preview(home: &Path, pid: &str, wid: &str, target: Option<String>) -> Result<Value> {
    let lib = Library::open(home)?;
    let w = item(find(&lib, pid)?, wid)?;
    let d = w.deliveries.last().context("事项还没有交付记录")?;
    let empty = |error: String| {
        json!({"target":"","branches":[],"candidate":"","stat":"","diff":"","truncated":false,
               "digest":"","files_changed":0,"insertions":0,"deletions":0,"error":error})
    };
    let candidate = match merge::candidate(home, d) {
        Ok(c) => c,
        Err(e) => return Ok(empty(format!("{e:#}"))),
    };
    let branches = merge::branches(&candidate)?;
    let target = target
        .or(candidate.default_target.clone())
        .or_else(|| {
            branches
                .iter()
                .find(|b| b.writable() && !b.internal)
                .map(|b| b.name.clone())
        })
        .context("没有可合并的目标分支")?;
    let list: Vec<Value> = branches
        .iter()
        .filter(|b| !b.internal)
        .map(|b| json!({"name":b.name,"status":b.status,"reason":b.reason,"writable":b.writable(),"checked_out":b.checked_out}))
        .collect();
    match merge::preview(home, d, &target) {
        Ok(p) => Ok(json!({
            "target": target, "branches": list, "candidate": candidate.sha,
            "stat": p.stat, "diff": p.diff, "truncated": p.truncated, "digest": p.diff_sha256,
            "files_changed": p.files_changed, "insertions": p.insertions, "deletions": p.deletions,
            "error": null,
        })),
        Err(e) => {
            let mut v = empty(format!("{e:#}"));
            v["target"] = json!(target);
            v["branches"] = json!(list);
            v["candidate"] = json!(candidate.sha);
            Ok(v)
        }
    }
}

pub fn merge_confirm(home: &Path, pid: &str, wid: &str, target: &str, digest: &str) -> Result<()> {
    let mut lib = Library::open(home)?;
    let run = item(find(&lib, pid)?, wid)?
        .deliveries
        .last()
        .context("事项还没有交付记录")?
        .run
        .clone();
    merge::merge(home, &mut lib, pid, wid, &run, target, digest)?;
    Ok(())
}

fn memory_dir(home: &Path, pid: &str) -> Result<PathBuf> {
    aep::workspace::ident(pid, 64)?;
    Ok(home.join("workflows").join(pid))
}

pub fn memories(home: &Path, pid: &str) -> Result<Value> {
    let notes = Memory::browse(&memory_dir(home, pid)?)?;
    Ok(json!(
        notes
            .iter()
            .map(|n| json!({
                "id": n["id"], "kind": n["kind"], "author": n["author"], "body": n["body"],
                "created_at": n["created_at"], "superseded": !n["successor"].is_null(),
            }))
            .collect::<Vec<_>>()
    ))
}

pub fn add_memory(home: &Path, pid: &str, text: &str) -> Result<i64> {
    let mut db = Memory::open(&memory_dir(home, pid)?)?;
    db.add_manual(&Key::new("", "", ""), "decision", text, None, None)
}

pub fn reveal(path: &str) -> Result<()> {
    let p = Path::new(path);
    if !p.exists() {
        bail!("路径不存在：{path}");
    }
    std::process::Command::new("open").arg(p).spawn()?;
    Ok(())
}

pub fn home() -> Result<PathBuf> {
    aep::project::catalog_dir().context("找不到项目库目录")
}

fn history_note(n: &aep::project::timeline::Note) -> Value {
    json!({"id": n.id, "kind": n.kind, "body": n.body})
}

pub fn history(home: &Path, pid: &str, wid: &str) -> Result<Value> {
    let lib = Library::open(home)?;
    let w = item(find(&lib, pid)?, wid)?;
    let t = aep::project::timeline::load(home, pid, w)?;
    let names = w.template.stages();
    let rounds: Vec<Value> = t
        .rounds
        .iter()
        .map(|r| {
            json!({
                "run": r.run,
                "stage": r.stage,
                "stage_name": r.stage.and_then(|s| names.get(s)).map_or("", |s| s.name),
                "status": r.status,
                "round": r.round,
                "attempt": r.attempt,
                "agents": r.agents.iter().map(|a| json!({
                    "role": a.role, "client": a.client, "model": a.model, "session": a.session,
                    "reads": a.reads.iter().map(history_note).collect::<Vec<_>>(),
                    "writes": a.writes.iter().map(history_note).collect::<Vec<_>>(),
                    "handoff": a.handoff,
                })).collect::<Vec<_>>(),
                "received": r.read_stage_transfer.as_ref().map(|x| json!({
                    "from_stage": x.from, "decision": x.decision, "acceptance": x.acceptance,
                })),
            })
        })
        .collect();
    let merges: Vec<Value> = t
        .merge_events
        .iter()
        .map(|m| json!({"target": m.target_branch, "sha": m.merged_sha.clone().unwrap_or(m.candidate_sha.clone()), "at": m.merged_at, "files_changed": m.files_changed}))
        .collect();
    Ok(json!({"rounds": rounds, "merges": merges, "errors": t.errors}))
}

pub fn handoffs(home: &Path, pid: &str, wid: &str) -> Result<Value> {
    let lib = Library::open(home)?;
    Ok(serde_json::to_value(
        &item(find(&lib, pid)?, wid)?.handoffs,
    )?)
}

pub fn send_handoff(home: &Path, pid: &str, wid: &str, to: &str, note: &str) -> Result<()> {
    change(home, |lib| find_mut(lib, pid)?.handoff(wid, to, note))
}

static ZCODE: std::sync::Mutex<Option<Vec<Value>>> = std::sync::Mutex::new(None);

pub fn zcode_models(refresh: bool) -> Result<Value> {
    if !refresh && let Some(cached) = ZCODE.lock().unwrap().clone() {
        return Ok(json!(cached));
    }
    let models: Vec<Value> = aep::zcode_catalog::discover(&runner::path()?)?
        .iter()
        .map(|m| {
            let provider = m.provider_label.clone().unwrap_or(m.id.provider_id.clone());
            json!({"label": format!("{provider} · {}", m.label), "selection": m.default_selection()})
        })
        .collect();
    *ZCODE.lock().unwrap() = Some(models.clone());
    Ok(json!(models))
}

fn repo_data(path: &str) -> Result<PathBuf> {
    Ok(Workspace::open(Path::new(path))?.data)
}

pub fn repo_memories(path: &str) -> Result<Value> {
    let notes = Memory::browse(&repo_data(path)?)?;
    Ok(json!(
        notes
            .iter()
            .map(|n| json!({
                "id": n["id"], "kind": n["kind"], "author": n["author"], "body": n["body"],
                "created_at": n["created_at"], "superseded": !n["successor"].is_null(),
            }))
            .collect::<Vec<_>>()
    ))
}

pub fn add_repo_memory(path: &str, text: &str) -> Result<i64> {
    Memory::open(&repo_data(path)?)?.add_manual(&Key::new("", "", ""), "decision", text, None, None)
}
