//! Studio commands as plain functions over the catalog directory. Shapes follow
//! `studio/src/api/types.ts`.
use crate::runner;
use aep::{
    autopilot,
    flow::{self, Flow},
    handoff::{self, Assignment, busy},
    memory::{Key, Memory, TEXT_LIMIT},
    project::{Kind, Library, Project, Work, WorkFlow, import, merge, shelf},
    session_reuse,
    transcript::{self, Entry},
    usage,
    work_draft::DraftFlow,
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

fn flow_view(p: &Project, flow: &Flow) -> Value {
    let revision = p
        .flows
        .iter()
        .find(|entry| entry.flow.id == flow.id)
        .map_or(0, |entry| entry.revision);
    let stages: Vec<Value> = flow
        .stages
        .iter()
        .map(|s| {
            json!({
                "name": s.name, "brief": s.brief, "role": s.role,
                "skippable": s.skippable, "back_to": s.back_to,
            })
        })
        .collect();
    let warnings: Vec<_> = flow::validate(flow, &[])
        .warnings
        .into_iter()
        .map(|issue| issue.message)
        .collect();
    json!({
        "id": flow.id, "label": flow.label, "builtin": flow::builtin_by_id(&flow.id).is_some(),
        "stages": stages, "in_use": p.flow_in_use(&flow.id), "revision": revision,
        "warnings": warnings,
    })
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
                    let stages = w.stages();
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
                        "flow_error": w.flow_error.is_some(),
                        "status": status,
                        "running": running_attempt(home, &p.id, w).is_some(),
                        "autopilot": autopilot::running(&autopilot_dir(home, &p.id, &w.id)),
                    })
                })
                .collect();
            let flows: Vec<_> = p
                .flow_choices()
                .into_iter()
                .map(|f| flow_view(p, f))
                .collect();
            let broken_flows: Vec<_> = p
                .broken_flows
                .iter()
                .map(|entry| {
                    json!({
                        "id": entry.raw["id"].as_str(), "label": entry.raw["label"].as_str(),
                        "error": entry.error,
                    })
                })
                .collect();
            json!({
                "id": p.id, "name": p.name, "repositories": p.repositories, "work": work,
                "flows": flows, "broken_flows": broken_flows,
                "workspace": p.workspace_root.as_ref().map(|root| json!({
                    "root": root,
                    "exists": root.is_dir(),
                    "notes": aep::project::folder::notes(root)
                        .into_iter()
                        .map(|n| n.file_name().unwrap_or_default().to_string_lossy().into_owned())
                        .collect::<Vec<_>>(),
                })),
            })
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
    let templates: Vec<_> = [Template::Software, Template::Game]
        .into_iter()
        .map(|t| DraftFlow::from(flow::builtin(t)))
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
    let trimmed = Path::new(path.trim());
    match Workspace::open(trimmed) {
        Ok(ws) => Ok(ws.root.canonicalize()?),
        // The folder scanner explains what the path is instead of git's raw `fatal`.
        Err(_) => Err(aep::project::folder::unreadable(trimmed)),
    }
}

/// Classify a picked folder for the repository list UI: one repository, a folder of
/// repositories, or a path that cannot be used. `depth` defaults to `DEFAULT_DEPTH`.
pub fn resolve_folder(path: &str, depth: Option<u8>) -> Result<Value> {
    let resolved = aep::project::folder::resolve(
        Path::new(path.trim()),
        depth.unwrap_or(aep::project::folder::DEFAULT_DEPTH),
    )?;
    let message = aep::project::folder::message(&resolved);
    let mut value = serde_json::to_value(&resolved)?;
    value["message"] = json!(message);
    Ok(value)
}

/// Create a project and return its ID. All repositories are validated before anything is
/// written, so a bad path fails the whole call and the error names the offending folder.
pub fn create_project(
    home: &Path,
    name: &str,
    repositories: &[String],
    workspace_root: Option<&str>,
) -> Result<String> {
    let repos = repositories
        .iter()
        .map(|r| repository(r).map_err(|err| name_repository(r, err)))
        .collect::<Result<Vec<_>>>()?;
    let root = workspace_root
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(Path::new)
        .map(|p| {
            let root = p
                .canonicalize()
                .with_context(|| format!("工作区文件夹无法读取：{}", p.display()))?;
            ensure!(root.is_dir(), "工作区根不是文件夹：{}", root.display());
            Ok(root)
        })
        .transpose()?;
    change(home, |lib| {
        lib.create(name, repos.clone())?;
        let p = lib
            .projects
            .iter_mut()
            .find(|p| p.name == name.trim())
            .context("项目创建失败")?;
        p.workspace_root = root.clone();
        Ok(p.id.clone())
    })
}

/// Attach several repositories to a project in one all-or-nothing change. Repositories the
/// project already has are reported as `existing`; the workspace root follows Q3 (the first
/// one wins, a different one only yields `kept_root`).
pub fn add_repositories(
    home: &Path,
    pid: &str,
    paths: &[String],
    workspace_root: Option<&str>,
) -> Result<Value> {
    let repos = paths
        .iter()
        .map(|r| repository(r).map_err(|err| name_repository(r, err)))
        .collect::<Result<Vec<_>>>()?;
    let root = workspace_root
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(Path::new)
        .map(|p| {
            let root = p
                .canonicalize()
                .with_context(|| format!("工作区文件夹无法读取：{}", p.display()))?;
            ensure!(root.is_dir(), "工作区根不是文件夹：{}", root.display());
            Ok(root)
        })
        .transpose()?;
    change(home, |lib| {
        let p = find_mut(lib, pid)?;
        let (added, existing): (Vec<PathBuf>, Vec<PathBuf>) = repos
            .iter()
            .cloned()
            .partition(|repo| !p.repositories.contains(repo));
        for repo in &repos {
            if !p.repositories.contains(repo) {
                p.repositories.push(repo.clone());
            }
        }
        let mut kept_root = None;
        match (&p.workspace_root, &root) {
            (None, Some(new)) => p.workspace_root = Some(new.clone()),
            (Some(kept), Some(new)) if kept != new => kept_root = Some(kept.clone()),
            _ => {}
        }
        Ok(json!({
            "added": added, "existing": existing,
            "workspace_root": p.workspace_root, "kept_root": kept_root,
        }))
    })
}

/// Prefix a repository error with the folder's own name so the dialog can point at the row.
fn name_repository(path: &str, err: anyhow::Error) -> anyhow::Error {
    let name = Path::new(path.trim())
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.trim().to_owned());
    anyhow::anyhow!("{name}：{err:#}")
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

pub fn save_flow(home: &Path, pid: &str, mut flow: Value) -> Result<Value> {
    if let Some(fields) = flow.as_object_mut() {
        fields.entry("id").or_insert_with(|| json!(""));
    }
    let flow: Flow = serde_json::from_value(flow)?;
    let (id, warnings) = change(home, |lib| find_mut(lib, pid)?.save_flow(flow.clone()))?;
    let warnings: Vec<_> = warnings.into_iter().map(|issue| issue.message).collect();
    Ok(json!({"id": id, "warnings": warnings}))
}

pub fn copy_flow(home: &Path, pid: &str, id: &str, label: Option<&str>) -> Result<String> {
    change(home, |lib| find_mut(lib, pid)?.copy_flow(id, label))
}

pub fn delete_flow(home: &Path, pid: &str, id: &str) -> Result<usize> {
    change(home, |lib| find_mut(lib, pid)?.delete_flow(id))
}

pub fn create_work(
    home: &Path,
    pid: &str,
    name: &str,
    template: &str,
    release: bool,
    repositories: &[String],
) -> Result<String> {
    let kind = if release {
        Kind::Release
    } else {
        Kind::Feature
    };
    change(home, |lib| {
        lib.active = Some(pid.into());
        let p = find_mut(lib, pid)?;
        p.add_work_flow(name, kind, template)?;
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
    let req = read_request(&dir);
    let summary = usage::summary(&dir);
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
        "route_back_rejected": ev["route_back_rejected"],
        "committed": ev["committed"] == true,
        "head_sha": ev["head_sha"],
        "tokens": summary.tokens.total(),
        "session": session_label(&dir, &ev, req.as_ref()),
        "launch": object_or_null(&ev["launch"]),
        "continuable": continuable(req.as_ref().map(|r| r.profile.client), &ev),
        "count_maybe_high": summary.count_maybe_high,
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
    let diet_totals = diet_totals(home, p);
    let mut budget_used: std::collections::BTreeMap<usize, u64> = Default::default();
    for a in &w.attempts {
        *budget_used.entry(a.stage).or_default() +=
            usage::summary(&workflow::attempt_dir(home, pid, wid, &a.id))
                .tokens
                .total();
    }
    let stages: Vec<Value> = w
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
                "budget": {
                    "limit": w.budgets.get(&i).copied().or(p.stage_budget),
                    "own": w.budgets.get(&i),
                    "used": budget_used.get(&i).copied().unwrap_or(0),
                    "suggested": diet_totals.get(&s.name).map_or(Value::Null, |t| suggested_budget(t)),
                },
                "session_preview": if state == "current" && running.is_none() && !w.delivered() {
                    session_preview(home, p, w, i)
                } else {
                    Value::Null
                },
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
            json!({"run": d.run, "archived_at": d.archived_at, "merged": merged, "pull_request": d.pull_request})
        })
        .collect();
    let draft = (w.delivered() && w.shelved.is_none())
        .then(|| w.memory_draft(home, pid).ok())
        .flatten();
    let flow = w.flow().ok();
    let flow_view = flow.map(|flow| {
        let source_state = w.flow.as_ref().map(|snapshot| {
            match p.flows.iter().find(|entry| entry.flow.id == flow.id) {
                Some(source) if source.revision == snapshot.source_revision => "same",
                Some(_) => "changed",
                None => "deleted",
            }
        });
        json!({
            "id": flow.id, "label": flow.label, "builtin": w.flow.is_none(),
            "source_state": source_state,
        })
    });
    Ok(json!({
        "project": pid, "id": w.id, "name": w.name, "kind": kind_id(w.kind),
        "template": flow::template_id(w.template),
        "template_label": flow.map_or("流程无法读取", |f| f.label.as_str()),
        "flow": flow_view,
        "flow_error": w.flow_error.as_ref().map(|broken| &broken.error),
        "stage": w.stage, "delivered": w.delivered(), "shelved": w.shelved,
        "repositories": w.repositories, "branch": branch, "worktrees": trees,
        "upstream": w.repositories.first().and_then(|r| p.upstreams.get(r)),
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
        "budget": {
            "total_limit": w.budget_total,
            "total_used": budget_used.values().sum::<u64>(),
            "stage_default": p.stage_budget,
        },
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
    let used = usage::measure(&dir).unwrap_or_default();
    let limit = read_request(&dir).and_then(|r| r.token_limit);
    // Codex counts live only from its rollout (found and remembered by `measure`); without
    // it the CLI reports usage once the turn ends, so a budget cannot stop it midway (R8).
    let live_usage = ev["profile"]["client"] != "Codex" || dir.join("rollout.path").is_file();
    Ok(json!({
        "usage": used.summary,
        "token_limit": limit,
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
        "live_usage": live_usage,
        "launch": object_or_null(&ev["launch"]),
        "usage_window": object_or_null(&ev["usage_window"]),
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
        ensure!(stage < w.flow()?.stages.len(), "阶段不存在");
        if let Some(n) = &note {
            if stage >= w.notes.len() {
                w.notes.resize_with(stage + 1, String::new);
            }
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
    ensure!(stage < w.flow()?.stages.len(), "阶段不存在");
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
            brief: w.notes.get(stage).cloned().unwrap_or_default(),
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
    let name = &w.flow()?.stages.get(from).context("阶段不存在")?.name;
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
        format!("「{name}」未通过")
    } else {
        reason
    };
    let message = match &report {
        Some(d) => autopilot::rework_message(name, &reason, d),
        None => format!("「{name}」未通过，需要你先解决：\n{reason}"),
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
                w.flow()?.skippable(stage) && stage > w.stage,
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
        ensure!(stage < w.flow()?.stages.len(), "阶段不存在");
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
    let stages = &w.flow()?.stages;
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
    change(home, |lib| {
        let p = find_mut(lib, pid)?;
        let source = item(p, wid)?.import_source().map(str::to_owned);
        p.remove_work(home, wid)?;
        // Deleting an imported item is a decision: the next sync must not bring it back.
        if let Some(branch) = source
            && !p.dismissed_imports.contains(&branch)
        {
            p.dismissed_imports.push(branch);
        }
        Ok(())
    })
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

/// A repository's remote setting (`configured`) and what Studio detects for it (`detected`).
pub fn upstream(home: &Path, pid: &str, repo: &str) -> Result<Value> {
    let lib = Library::open(home)?;
    let p = find(&lib, pid)?;
    let path = PathBuf::from(repo);
    ensure!(p.repositories.contains(&path), "仓库未关联到此项目");
    Ok(json!({
        "repository": path,
        "configured": p.upstreams.get(&path),
        "detected": aep::project::upstream::detect(&path),
    }))
}

/// Save (or with None remove) a repository's remote setting.
pub fn set_upstream(
    home: &Path,
    pid: &str,
    repo: &str,
    up: Option<aep::project::upstream::Upstream>,
) -> Result<Value> {
    let path = PathBuf::from(repo);
    if let Some(up) = &up {
        up.validate()?;
        ensure!(
            aep::workspace::git(&path, &["remote", "get-url", &up.remote]).is_ok(),
            "仓库没有名为 {} 的远端",
            up.remote
        );
    }
    let mut lib = Library::open(home)?;
    lib.change(home, |lib| {
        let p = find_mut(lib, pid)?;
        ensure!(p.repositories.contains(&path), "仓库未关联到此项目");
        match up {
            Some(up) => p.upstreams.insert(path.clone(), up),
            None => p.upstreams.remove(&path),
        };
        Ok(())
    })?;
    upstream(home, pid, repo)
}

/// Read-only preview of pushing the delivery and its PR (fetches the target branch).
pub fn pr_preview(home: &Path, pid: &str, wid: &str, branch: Option<String>) -> Result<Value> {
    let lib = Library::open(home)?;
    let w = item(find(&lib, pid)?, wid)?;
    let d = w.deliveries.last().context("事项还没有交付记录")?;
    Ok(serde_json::to_value(aep::project::upstream::preview(
        home,
        &lib,
        d,
        branch.as_deref(),
    )?)?)
}

/// Push the accepted commit and open (or update) the PR, after the person confirmed.
#[allow(clippy::too_many_arguments)]
pub fn pr_submit(
    home: &Path,
    pid: &str,
    wid: &str,
    branch: &str,
    title: &str,
    body: &str,
    expect: Option<String>,
    open: bool,
    number: Option<u64>,
) -> Result<Value> {
    let mut lib = Library::open(home)?;
    let pr = aep::project::upstream::submit(
        home,
        &mut lib,
        pid,
        wid,
        aep::project::upstream::Submit {
            branch,
            title,
            body,
            expect: expect.as_deref(),
            open,
            number,
        },
    )?;
    Ok(serde_json::to_value(pr)?)
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
            // Tombstones of notes taken out by a sync; the note itself shows as replaced.
            .filter(|n| n["scope"] != "retired")
            .map(|n| json!({
                "id": n["id"], "kind": n["kind"], "author": n["author"], "body": n["body"],
                "created_at": n["created_at"], "superseded": !n["successor"].is_null(),
                "replaces": n["replaces"],
            }))
            .collect::<Vec<_>>()
    ))
}

pub fn add_memory(home: &Path, pid: &str, text: &str) -> Result<i64> {
    let mut db = Memory::open(&memory_dir(home, pid)?)?;
    db.add_manual(&Key::new("", "", ""), "decision", text, None, None)
}

/// Save a new version of a project memory note; the old one stays in its history and
/// agents read only the newest version. Restoring an old version is saving its text again.
pub fn revise_memory(home: &Path, pid: &str, id: i64, text: &str) -> Result<i64> {
    let dir = memory_dir(home, pid)?;
    let notes = Memory::browse(&dir)?;
    let note = notes.iter().find(|n| n["id"] == id).context("记忆不存在")?;
    ensure!(
        note["successor"].is_null(),
        "这条记忆已有更新的版本，请在最新版本上修改"
    );
    ensure!(note["body"].as_str() != Some(text.trim()), "内容没有变化");
    let kind = match note["kind"].as_str() {
        Some("handoff") => aep::memory::Kind::Handoff,
        Some("open_question") => aep::memory::Kind::OpenQuestion,
        Some("review") => aep::memory::Kind::Review,
        _ => aep::memory::Kind::Decision,
    };
    Memory::open(&dir)?.revise(id, kind, text.trim())
}

/// What syncing a repository's Claude memory and worktree branches would change.
pub fn import_sync_plan(home: &Path, pid: &str, path: &str) -> Result<import::SyncPlan> {
    let lib = Library::open(home)?;
    find(&lib, pid)?.sync_plan(home, &repository(path)?)
}

/// Make the project match its sources (see `Project::sync`); returns what changed.
pub fn import_sync(
    home: &Path,
    pid: &str,
    path: &str,
    memories: bool,
    items: bool,
) -> Result<import::SyncPlan> {
    let repo = repository(path)?;
    change(home, |lib| {
        find_mut(lib, pid)?.sync(home, &repo, memories, items)
    })
}

/// A repository's branches and recent commit graph, with each `aep-stage/<id>` branch
/// labelled by the project item it belongs to.
pub fn branch_graph(home: &Path, pid: &str, path: &str, limit: usize) -> Result<Value> {
    let lib = Library::open(home)?;
    let p = find(&lib, pid)?;
    let graph = aep::project::branches::graph(&repository(path)?, limit.clamp(50, 2000))?;
    let mut v = serde_json::to_value(&graph)?;
    for b in v["branches"].as_array_mut().into_iter().flatten() {
        let name = b["name"].as_str().unwrap_or_default().to_owned();
        let item = name.strip_prefix("aep-stage/").and_then(|rest| {
            p.work
                .iter()
                .find(|w| rest == w.id || rest.starts_with(&format!("{}-", w.id)))
        });
        b["item"] = item.map_or(Value::Null, |w| json!({"id": w.id, "name": w.name}));
    }
    Ok(v)
}

/// External git tools Studio can open a repository or worktree in: known apps that are
/// installed, plus any listed in `<catalog>/plugins/git-tools.json` as
/// `[{"label": "...", "command": ["program", "arg", "{path}"]}]`.
fn tool_list(home: &Path) -> Vec<(String, String, Vec<String>)> {
    let mut tools = Vec::new();
    for (app, label) in [
        ("Fork", "Fork"),
        ("GitHub Desktop", "GitHub Desktop"),
        ("Sourcetree", "Sourcetree"),
        ("Tower", "Tower"),
        ("GitKraken", "GitKraken"),
        ("Visual Studio Code", "VS Code（Git Graph 扩展）"),
        ("Cursor", "Cursor"),
    ] {
        if Path::new(&format!("/Applications/{app}.app")).is_dir() {
            tools.push((
                format!("app:{app}"),
                label.to_owned(),
                vec![
                    "/usr/bin/open".into(),
                    "-a".into(),
                    app.into(),
                    "{path}".into(),
                ],
            ));
        }
    }
    let on_path = |bin: &str| {
        runner::path().ok().and_then(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join(bin))
                .find(|f| f.is_file())
        })
    };
    for (bin, label) in [("lazygit", "lazygit"), ("tig", "tig")] {
        if let Some(exe) = on_path(bin) {
            // Terminal UIs open in a new Terminal window.
            let script = format!(
                "tell application \"Terminal\" to do script \"cd \" & quoted form of \"{{path}}\" & \" && {}\"",
                exe.display()
            );
            tools.push((
                format!("tui:{bin}"),
                label.to_owned(),
                vec![
                    "/usr/bin/osascript".into(),
                    "-e".into(),
                    script,
                    "-e".into(),
                    "tell application \"Terminal\" to activate".into(),
                ],
            ));
        }
    }
    if let Some(exe) = on_path("gitk") {
        tools.push((
            "bin:gitk".into(),
            "gitk".into(),
            vec![exe.display().to_string(), "--all".into()],
        ));
    }
    let custom = home.join("plugins").join("git-tools.json");
    if let Ok(list) = aep::workspace::read(&custom) {
        for (i, t) in list.as_array().into_iter().flatten().enumerate() {
            let label = t["label"].as_str().unwrap_or_default().to_owned();
            let command: Vec<String> = t["command"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|a| a.as_str().map(str::to_owned))
                .collect();
            if !label.is_empty() && !command.is_empty() {
                tools.push((format!("custom:{i}"), label, command));
            }
        }
    }
    tools
}

pub fn git_tools(home: &Path) -> Result<Value> {
    Ok(json!({
        "tools": tool_list(home)
            .into_iter()
            .map(|(id, label, _)| json!({"id": id, "label": label}))
            .collect::<Vec<_>>(),
        "config": home.join("plugins").join("git-tools.json"),
    }))
}

pub fn open_git_tool(home: &Path, id: &str, path: &str) -> Result<()> {
    let dir = Path::new(path);
    ensure!(dir.is_dir(), "目录不存在：{path}");
    let (_, label, command) = tool_list(home)
        .into_iter()
        .find(|(i, ..)| i == id)
        .context("找不到这个 git 工具")?;
    let args: Vec<String> = command.iter().map(|a| a.replace("{path}", path)).collect();
    std::process::Command::new(&args[0])
        .args(&args[1..])
        .current_dir(dir)
        .env("PATH", runner::path()?)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("无法启动 {label}"))?;
    Ok(())
}

fn tidy_dir(home: &Path, pid: &str) -> Result<PathBuf> {
    Ok(memory_dir(home, pid)?.join("memory-tidy"))
}

/// Start a memory tidy-up with a model CLI; returns the job id.
pub fn tidy_start(
    home: &Path,
    pid: &str,
    client: &str,
    model: &str,
    instruction: &str,
) -> Result<String> {
    ensure!(
        !tidy_status(home, pid)?["status"]
            .as_str()
            .is_some_and(|s| s == "running" || s == "starting"),
        "已有一次整理在进行"
    );
    let id = format!("{}", chrono_stamp());
    let job = tidy_dir(home, pid)?.join(&id);
    fs::create_dir_all(&job)?;
    let profile = one_shot_profile(client, model)?;
    aep::project::save_json(
        &job,
        "job.json",
        &json!({"profile": profile, "instruction": instruction.trim()}),
    )?;
    aep::project::save_json(&job, "status.json", &json!({"status": "starting"}))?;
    runner::memory_tidy(home, pid, &job)?;
    Ok(id)
}

fn chrono_stamp() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

/// The latest background CLI job in `dir` (tidy or draft): status, error, spec, result.
fn latest_job(dir: &Path, result: &str) -> Value {
    let latest = fs::read_dir(dir)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .max_by_key(|n| n.parse::<u128>().unwrap_or(0));
    let Some(id) = latest else {
        return json!({"id": null, "status": null});
    };
    let job = dir.join(&id);
    let status = aep::workspace::read(&job.join("status.json")).unwrap_or_default();
    let spec = aep::workspace::read(&job.join("job.json")).unwrap_or_default();
    let mut s = status["status"].as_str().unwrap_or("starting").to_owned();
    // A job whose process died before it began.
    let age = job
        .join("status.json")
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .unwrap_or_default();
    if s == "starting" && age > Duration::from_secs(30) {
        s = "failed".into();
    }
    json!({
        "id": id,
        "status": s,
        "error": status["error"].as_str().or_else(|| (s == "failed" && status["error"].is_null()).then_some("后台进程没有启动")),
        "client": spec["profile"]["client"],
        "model": spec["profile"]["model"],
        "instruction": spec["instruction"],
        "request": spec["request"],
        "applied": status["applied"],
        "result": aep::workspace::read(&job.join(result)).ok(),
    })
}

/// The latest tidy job: status, error, and its proposal once ready.
pub fn tidy_status(home: &Path, pid: &str) -> Result<Value> {
    let mut v = latest_job(&tidy_dir(home, pid)?, "proposal.json");
    v["proposal"] = v["result"].take();
    Ok(v)
}

fn one_shot_profile(client: &str, model: &str) -> Result<Profile> {
    let client = match client {
        "Codex" => Client::Codex,
        "Claude" => Client::Claude,
        _ => bail!("目前支持 Codex 和 Claude"),
    };
    Ok(Profile {
        client,
        executable: String::new(),
        model: model.trim().to_owned(),
        check_command: String::new(),
    })
}

/// Draft work items from a description with a model CLI that reads the repository.
pub fn draft_start(
    home: &Path,
    pid: &str,
    client: &str,
    model: &str,
    request: &str,
    repo_path: Option<&str>,
) -> Result<String> {
    ensure!(!request.trim().is_empty(), "请描述要做的事");
    let profile = one_shot_profile(client, model)?;
    let lib = Library::open(home)?;
    let p = find(&lib, pid)?;
    let repo = match repo_path.filter(|r| !r.trim().is_empty()) {
        Some(r) => Some(repository(r)?),
        None => p.repositories.first().cloned(),
    };
    let dir = memory_dir(home, pid)?.join("work-drafts");
    ensure!(
        !latest_job(&dir, "draft.json")["status"]
            .as_str()
            .is_some_and(|s| s == "running" || s == "starting"),
        "已有一次起草在进行"
    );
    let notes: Vec<Value> = aep::memory_tidy::current(&memory_dir(home, pid)?)?
        .into_iter()
        .map(|n| json!({"id": n["id"], "body": n["body"]}))
        .collect();
    let existing: Vec<&str> = p
        .work
        .iter()
        .filter(|w| w.shelved.is_none())
        .map(|w| w.name.as_str())
        .collect();
    let flows: Vec<_> = p.flow_choices().into_iter().map(DraftFlow::from).collect();
    let id = chrono_stamp().to_string();
    let job = dir.join(&id);
    fs::create_dir_all(&job)?;
    aep::project::save_json(
        &job,
        "job.json",
        &json!({
            "profile": profile, "request": request.trim(), "repository": repo,
            "notes": notes, "existing": existing, "flows": flows,
        }),
    )?;
    aep::project::save_json(&job, "status.json", &json!({"status": "starting"}))?;
    runner::work_draft(&job)?;
    Ok(id)
}

pub fn draft_status(home: &Path, pid: &str) -> Result<Value> {
    let mut v = latest_job(&memory_dir(home, pid)?.join("work-drafts"), "draft.json");
    v["draft"] = v["result"].take();
    Ok(v)
}

/// Apply the chosen operations (by index) of a tidy proposal.
pub fn tidy_apply(home: &Path, pid: &str, job: &str, ops: &[usize]) -> Result<usize> {
    aep::workspace::ident(job, 64)?;
    let dir = tidy_dir(home, pid)?.join(job);
    let proposal: aep::memory_tidy::Proposal =
        serde_json::from_value(aep::workspace::read(&dir.join("proposal.json"))?)?;
    let chosen: Vec<_> = ops
        .iter()
        .map(|&i| proposal.ops.get(i).cloned().context("方案里没有这个操作"))
        .collect::<Result<_>>()?;
    ensure!(!chosen.is_empty(), "请至少选择一项");
    let n = aep::memory_tidy::apply(&memory_dir(home, pid)?, &chosen)?;
    let mut status = aep::workspace::read(&dir.join("status.json")).unwrap_or_default();
    status["applied"] = json!(ops);
    aep::project::save_json(&dir, "status.json", &status)?;
    Ok(n)
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
    // Run requests preserve the stage definitions needed to display a damaged item's history.
    let historical_work = if w.flow_error.is_some() {
        w.attempts.iter().rev().find_map(|attempt| {
            aep::workspace::ident(&attempt.id, 64).ok()?;
            let dir = workflow::attempt_dir(home, pid, wid, &attempt.id);
            let request = read_request(&dir)?;
            let mut view = w.clone();
            view.flow_error = None;
            view.flow = Some(WorkFlow {
                flow: request.flow().clone(),
                source_revision: 0,
            });
            Some(view)
        })
    } else {
        None
    };
    let w = historical_work.as_ref().unwrap_or(w);
    let t = aep::project::timeline::load(home, pid, w)?;
    let names = w.stages();
    let rounds: Vec<Value> = t
        .rounds
        .iter()
        .map(|r| {
            json!({
                "run": r.run,
                "stage": r.stage,
                "stage_name": r.stage.and_then(|s| names.get(s)).map_or("", |s| s.name.as_str()),
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
                "replaces": n["replaces"],
            }))
            .collect::<Vec<_>>()
    ))
}

pub fn add_repo_memory(path: &str, text: &str) -> Result<i64> {
    Memory::open(&repo_data(path)?)?.add_manual(&Key::new("", "", ""), "decision", text, None, None)
}

fn parse_tokens(v: Option<u64>) -> Option<u64> {
    v.filter(|n| *n > 0)
}

/// Set a stage's token budget, or (stage None) the item's total; None or 0 removes it.
pub fn set_budget(
    home: &Path,
    pid: &str,
    wid: &str,
    stage: Option<usize>,
    tokens: Option<u64>,
) -> Result<()> {
    change(home, |lib| {
        let w = find_mut(lib, pid)?.work_by_id_mut(wid)?;
        match stage {
            Some(i) => {
                ensure!(i < w.flow()?.stages.len(), "阶段不存在");
                match parse_tokens(tokens) {
                    Some(n) => w.budgets.insert(i, n),
                    None => w.budgets.remove(&i),
                };
            }
            None => w.budget_total = parse_tokens(tokens),
        }
        Ok(())
    })
}

/// The project's default per-stage budget.
pub fn set_project_budget(home: &Path, pid: &str, tokens: Option<u64>) -> Result<()> {
    change(home, |lib| {
        find_mut(lib, pid)?.stage_budget = parse_tokens(tokens);
        Ok(())
    })
}

/// Raise the budget that stopped a stage by `extra` tokens and continue its session.
pub fn extend_budget(
    home: &Path,
    pid: &str,
    wid: &str,
    stage: usize,
    extra: u64,
    message: &str,
) -> Result<String> {
    ensure!(extra > 0, "请填写要追加的 Token 数");
    let lib = Library::open(home)?;
    let p = find(&lib, pid)?;
    let w = item(p, wid)?;
    let b = usage::budget(home, p, w, stage);
    change(home, |lib| {
        let w = find_mut(lib, pid)?.work_by_id_mut(wid)?;
        if let Some(l) = b.stage_limit {
            w.budgets.insert(stage, l.max(b.stage_used) + extra);
        }
        if let Some(l) = b.total_limit
            && b.total_used + extra > l
        {
            w.budget_total = Some(l.max(b.total_used) + extra);
        }
        Ok(())
    })?;
    let text = message.trim();
    follow_up(
        home,
        pid,
        wid,
        stage,
        if text.is_empty() {
            "预算已追加。请从中断的地方继续完成本阶段的任务，不要重复已经做完的步骤。"
        } else {
            text
        },
    )
}

/// Where one attempt's tokens went.
pub fn usage_analysis(home: &Path, pid: &str, wid: &str, attempt: &str) -> Result<Value> {
    aep::workspace::ident(attempt, 64)?;
    let dir = workflow::attempt_dir(home, pid, wid, attempt);
    ensure!(dir.is_dir(), "执行记录不存在");
    let report = usage::measure(&dir)?;
    let ev = read_evidence(&dir);
    let mut v = usage::analysis(&report);
    v["summary"] = json!(report.summary);
    v["launch"] = object_or_null(&ev["launch"]);
    v["reused"] = json!(ev["reused_session"].is_object());
    v["window_calls"] = json!(
        ev["usage_window"]["calls"]
            .as_u64()
            .unwrap_or(report.summary.calls as u64)
    );
    Ok(v)
}

/// What a stage produced (the 产出 view): per run with a pinned commit (newest first), the
/// commits it made on the item branch and their size, and the spec documents those commits
/// or its evidence name, read at that run's commit. Platform-only merges of the base branch
/// are left out; a stage commit that also concluded such a merge shows only what was done on
/// top of the automatic merge (`--remerge-diff`).
pub fn stage_output(home: &Path, pid: &str, wid: &str, stage: usize) -> Result<Value> {
    let lib = Library::open(home)?;
    let p = find(&lib, pid)?;
    let w = item(p, wid)?;
    let mut docs: Vec<Value> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut runs = Vec::new();
    for a in w.attempts.iter().rev().filter(|a| a.stage == stage) {
        let dir = workflow::attempt_dir(home, pid, wid, &a.id);
        let ev = read_evidence(&dir);
        let req = read_request(&dir);
        let (Some(head), Some(repo)) = (
            ev["head_sha"].as_str(),
            req.as_ref().and_then(|r| r.repository.clone()),
        ) else {
            continue;
        };
        let commits = run_commits(&repo, head, &a.id).unwrap_or_default();
        let (mut files, mut insertions, mut deletions) = (Vec::<String>::new(), 0, 0);
        for c in &commits {
            for (path, add, del) in &c.files {
                insertions += add;
                deletions += del;
                if !files.contains(path) {
                    files.push(path.clone());
                }
            }
        }
        let specs = ev["spec_files"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .chain(files.iter().filter(|f| f.starts_with("specs/")).cloned());
        for path in specs {
            if seen.insert(path.clone())
                && let Ok(content) = git_text(&repo, &["show", &format!("{head}:{path}")])
            {
                docs.push(json!({"path":path,"content":content,"attempt":a.id,"finished_at":ev["finished_at"]}));
            }
        }
        let subtask = req
            .as_ref()
            .and_then(|r| r.subtask.as_ref())
            .map(|t| json!({"id":t.id,"title":t.title}));
        runs.push(json!({
            "attempt": a.id,
            "status": ev["status"],
            "follow_up": ev["follow_up"].is_object(),
            "subtask": subtask,
            "finished_at": ev["finished_at"],
            "head": head,
            "commits": commits.iter().map(|c| json!({"sha":c.sha,"title":c.title,"merge":c.merge})).collect::<Vec<_>>(),
            "files": files,
            "insertions": insertions,
            "deletions": deletions,
        }));
    }
    docs.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    Ok(json!({"stage":stage,"docs":docs,"runs":runs}))
}

/// The patch of one run's commits (see `stage_output`), capped like the merge preview.
pub fn stage_diff(home: &Path, pid: &str, wid: &str, attempt: &str) -> Result<Value> {
    aep::workspace::ident(attempt, 64)?;
    let dir = workflow::attempt_dir(home, pid, wid, attempt);
    ensure!(dir.is_dir(), "执行记录不存在");
    let ev = read_evidence(&dir);
    let head = ev["head_sha"].as_str().context("这次执行没有提交记录")?;
    let repo = read_request(&dir)
        .and_then(|r| r.repository)
        .context("这次执行没有关联仓库")?;
    const LIMIT: usize = 800_000;
    let mut diff = String::new();
    for c in run_commits(&repo, head, attempt)?.iter().rev() {
        let mut args = vec!["show", "--no-color", "--format=commit %h %s%n"];
        if c.merge {
            args.push("--remerge-diff");
        }
        args.push(&c.sha);
        diff.push_str(&git_text(&repo, &args)?);
        if diff.len() > LIMIT {
            break;
        }
    }
    let truncated = diff.len() > LIMIT;
    if truncated {
        let mut end = LIMIT;
        while !diff.is_char_boundary(end) {
            end -= 1;
        }
        diff.truncate(end);
    }
    Ok(json!({"attempt":attempt,"diff":diff,"truncated":truncated}))
}

struct RunCommit {
    sha: String,
    title: String,
    merge: bool,
    files: Vec<(String, u64, u64)>,
}

/// Commits a run made (they carry its `AEP-Run:` trailer), newest first, without the
/// platform's own merges of the base branch.
fn run_commits(repo: &Path, head: &str, attempt: &str) -> Result<Vec<RunCommit>> {
    let log = git_text(
        repo,
        &[
            "log",
            "-n",
            "40",
            "--format=%H%x1f%P%x1f%s",
            "--fixed-strings",
            &format!("--grep=AEP-Run: {attempt}"),
            head,
        ],
    )?;
    let mut out = Vec::new();
    for line in log.lines() {
        let mut f = line.split('\u{1f}');
        let (Some(sha), Some(parents), Some(title)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        let merge = parents.split_whitespace().count() > 1;
        // `合入 main（N 个新提交）`: the platform's merge before a stage, not the stage's work.
        if merge && title.starts_with("合入 ") {
            continue;
        }
        let mut args = vec!["show", "--numstat", "--format="];
        if merge {
            args.push("--remerge-diff");
        }
        args.push(sha);
        let files = git_text(repo, &args)?
            .lines()
            .filter_map(|l| {
                let mut p = l.splitn(3, '\t');
                let add = p.next()?.parse().unwrap_or(0);
                let del = p.next()?.parse().unwrap_or(0);
                Some((p.next()?.to_owned(), add, del))
            })
            .collect();
        out.push(RunCommit {
            sha: sha.to_owned(),
            title: title.to_owned(),
            merge,
            files,
        });
    }
    Ok(out)
}

fn git_text(repo: &Path, args: &[&str]) -> Result<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()?;
    ensure!(
        out.status.success(),
        "git {}：{}",
        args.first().unwrap_or(&""),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Token use across the project: totals, per item, per stage, per CLI/model, per day, the
/// heaviest runs and the account quota each CLI reported last.
pub fn project_usage(home: &Path, pid: &str) -> Result<Value> {
    let lib = Library::open(home)?;
    let p = find(&lib, pid)?;
    #[derive(Default)]
    struct Sum {
        tokens: usage::Tokens,
        cost: f64,
        runs: usize,
    }
    fn add(m: &mut Vec<(String, Sum)>, key: &str, s: &usage::Summary) {
        let i = m.iter().position(|(k, _)| k == key).unwrap_or_else(|| {
            m.push((key.to_owned(), Sum::default()));
            m.len() - 1
        });
        let e = &mut m[i].1;
        e.tokens.add(&s.tokens);
        e.cost += s.cost_usd.unwrap_or(0.0);
        e.runs += 1;
    }
    let row = |(k, s): &(String, Sum)| json!({"key": k, "tokens": s.tokens, "total": s.tokens.total(), "cost_usd": s.cost, "runs": s.runs});
    let mut total = Sum::default();
    let (mut items, mut stages, mut models, mut days) = (vec![], vec![], vec![], vec![]);
    let mut runs: Vec<Value> = vec![];
    let mut quota: std::collections::BTreeMap<String, (String, Value)> = Default::default();
    let (mut by_stage, mut by_model) = (Compare::default(), Compare::default());
    let mut maybe_high_runs = 0;
    for w in &p.work {
        let mut item_sum = Sum::default();
        for a in &w.attempts {
            let dir = workflow::attempt_dir(home, pid, &w.id, &a.id);
            let ev = read_evidence(&dir);
            let s = usage::summary(&dir);
            if s.tokens.total() == 0 {
                continue;
            }
            let stage = w.stages().get(a.stage).map_or("?", |s| s.name.as_str());
            let client = format!("{:?}", a.profile.client);
            let model = s
                .models
                .first()
                .cloned()
                .unwrap_or_else(|| a.profile.model.clone());
            let day = ev["started_at"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(10)
                .collect::<String>();
            add(&mut stages, stage, &s);
            add(&mut models, &format!("{client} · {model}"), &s);
            let after = diet(&ev);
            if matches!(a.profile.client, Client::Codex | Client::Claude)
                && ev["mode"] != "external"
            {
                by_stage.add(stage, after, &s);
                by_model.add(&format!("{client} · {model}"), after, &s);
            }
            if s.count_maybe_high {
                maybe_high_runs += 1;
            }
            add(&mut days, &day, &s);
            item_sum.tokens.add(&s.tokens);
            item_sum.cost += s.cost_usd.unwrap_or(0.0);
            item_sum.runs += 1;
            total.tokens.add(&s.tokens);
            total.cost += s.cost_usd.unwrap_or(0.0);
            total.runs += 1;
            if let Some(q) = &s.quota {
                let at = ev["started_at"].as_str().unwrap_or("").to_owned();
                if quota.get(&client).is_none_or(|(t, _)| *t < at) {
                    quota.insert(client.clone(), (at, q.clone()));
                }
            }
            runs.push(json!({
                "work": w.id, "work_name": w.name, "attempt": a.id, "stage": stage,
                "stage_index": a.stage, "client": client, "model": model,
                "status": ev["status"], "started_at": ev["started_at"],
                "tokens": s.tokens, "total": s.tokens.total(), "cost_usd": s.cost_usd, "calls": s.calls,
                "diet": after, "reused_from": ev["reused_session"]["name"].as_str(),
                "count_maybe_high": s.count_maybe_high, "launch": object_or_null(&ev["launch"]),
            }));
        }
        if item_sum.runs > 0 {
            items.push(json!({
                "key": w.id, "name": w.name, "shelved": w.shelved.is_some(),
                "tokens": item_sum.tokens, "total": item_sum.tokens.total(),
                "cost_usd": item_sum.cost, "runs": item_sum.runs,
                "budget": w.budget_total,
            }));
        }
    }
    items.sort_by_key(|v| std::cmp::Reverse(v["total"].as_u64().unwrap_or(0)));
    runs.sort_by_key(|v| std::cmp::Reverse(v["total"].as_u64().unwrap_or(0)));
    runs.truncate(12);
    stages.sort_by_key(|(_, s)| std::cmp::Reverse(s.tokens.total()));
    models.sort_by_key(|(_, s)| std::cmp::Reverse(s.tokens.total()));
    days.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(json!({
        "total": {"tokens": total.tokens, "total": total.tokens.total(), "cost_usd": total.cost, "runs": total.runs},
        "stage_budget": p.stage_budget,
        "resume_limit": p.resume_limit,
        "compare": {"by_stage": by_stage.rows(), "by_model": by_model.rows()},
        "maybe_high_runs": maybe_high_runs,
        "items": items,
        "stages": stages.iter().map(row).collect::<Vec<_>>(),
        "models": models.iter().map(row).collect::<Vec<_>>(),
        "days": days.iter().map(row).collect::<Vec<_>>(),
        "runs": runs,
        "quota": quota.into_iter().map(|(client, (at, q))| json!({"client": client, "at": at, "windows": q})).collect::<Vec<_>>(),
    }))
}

// ---- Token diet: session labels, launch config, before/after comparison (T7) ----

fn read_request(dir: &Path) -> Option<Request> {
    fs::read(dir.join("request.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
}

fn object_or_null(v: &Value) -> Value {
    if v.is_object() {
        v.clone()
    } else {
        Value::Null
    }
}

/// The compared value behind a "new session" reason, for "（前序 X）": the source run's CLI,
/// model (empty = default) or permission (sandbox when only that differs).
fn prior_value(
    reason: session_reuse::Reason,
    current: &session_reuse::Side,
    source: &session_reuse::Side,
) -> Value {
    use session_reuse::Reason as R;
    match reason {
        R::ClientDiffers => json!(format!("{:?}", source.client)),
        R::ModelDiffers => json!(source.model),
        R::PermissionDiffers if source.permission != current.permission => json!(source.permission),
        R::PermissionDiffers => json!(format!("{:?}", source.sandbox)),
        _ => Value::Null,
    }
}

/// How an attempt's session was chosen (interaction §2.1): reused from an adjacent stage,
/// continued from this stage, or new with a reason. Old records and ZCode / custom CLIs: null.
fn session_label(dir: &Path, ev: &Value, req: Option<&Request>) -> Value {
    let reused = &ev["reused_session"];
    if reused.is_object() {
        return json!({"kind": "reused", "stage": reused["stage"], "stage_name": reused["name"], "run": reused["run"]});
    }
    if ev["follow_up"]["resumed_session"].is_string() {
        return json!({"kind": "continued", "run": ev["follow_up"]["attempt"]});
    }
    let Some(reason) = ev["fresh_session_reason"]
        .as_str()
        .and_then(|r| serde_json::from_value::<session_reuse::Reason>(json!(r)).ok())
    else {
        return Value::Null;
    };
    if reason == session_reuse::Reason::ClientNotReusable {
        return Value::Null;
    }
    // The run compared with: the previous stage's run (adjacent reuse only).
    let prior = req
        .filter(|r| r.follow_up.is_none())
        .and_then(|r| {
            let (_, run) = r.prior_runs.last()?;
            aep::workspace::ident(run, 64).ok()?;
            let src = read_request(&dir.parent()?.join(run))?;
            Some(prior_value(
                reason,
                &session_reuse::Side::of(r, None),
                &session_reuse::Side::of(&src, None),
            ))
        })
        .unwrap_or(Value::Null);
    json!({
        "kind": "fresh", "reason": reason.as_str(),
        "context": ev["resume_context"], "threshold": ev["resume_threshold"], "prior": prior,
    })
}

/// Whether "add budget and continue" continues the original session: Claude and Codex
/// always can; ZCode only through an app-server session (R7).
fn continuable(client: Option<Client>, ev: &Value) -> bool {
    match client {
        Some(Client::Claude | Client::Codex) => true,
        Some(Client::ZCode) => {
            let n = &ev["native_session"];
            n["transport"] == "app-server"
                && n["session"].as_str().is_some_and(|s| !s.trim().is_empty())
        }
        _ => false,
    }
}

/// The profile a stage would run with now: an unset Codex / Claude model means the project's
/// preset, as `Project::reserve` fills it in.
fn effective_profile(p: &Project, w: &Work, stage: usize) -> Profile {
    let mut profile = w.profile(stage);
    if profile.model.trim().is_empty()
        && matches!(profile.client, Client::Codex | Client::Claude)
        && let Some(preset) = p
            .preset(profile.client)
            .filter(|x| !x.model.trim().is_empty())
    {
        profile.model = preset.model.clone();
    }
    profile
}

/// What the next run of the current stage would do with its session (interaction §4.1),
/// computed by the runner's own `session_reuse::decide`. Null for the first stage, ZCode and
/// custom CLIs. The workspace is assumed unchanged: a later change shows on the run's label.
fn session_preview(home: &Path, p: &Project, w: &Work, stage: usize) -> Value {
    use session_reuse::{Mode, Plan, Reason, Side};
    let stages = w.stages();
    let Some(st) = stages.get(stage) else {
        return Value::Null;
    };
    let profile = effective_profile(p, w, stage);
    let mut current = Side {
        stage,
        client: profile.client,
        model: profile.model.trim().to_owned(),
        sandbox: w.sandbox(stage),
        permission: session_reuse::permission(st.role),
        independent: st.role == Role::Accept,
        workspace: None,
    };
    let threshold = session_reuse::limit(p.resume_limit);
    // The next build subtask continues from the last one that passed, as the runner does.
    let subtask = (st.role == Role::Build && !autopilot::subtasks(home, &p.id, w).is_empty())
        .then(|| autopilot::finished_subtasks(home, &p.id, w, stage).pop())
        .flatten()
        .map(|d| (stage, format!("{} · {}", st.name, d.id), d.run));
    let source = subtask
        .or_else(|| {
            w.current_attempt
                .range(..stage)
                .next_back()
                .map(|(s, run)| {
                    let name = stages.get(*s).map_or("", |x| x.name.as_str());
                    (*s, name.to_owned(), run.clone())
                })
        })
        .map(|(s, name, run)| {
            let runs = workflow::attempt_dir(home, &p.id, &w.id, &run);
            session_reuse::source(runs.parent().unwrap_or(&runs), s, &name, &run)
        });
    let plan = match &source {
        Some(Err(_)) if matches!(current.client, Client::Codex | Client::Claude) => Plan::Fresh {
            reason: Reason::SessionUnavailable,
            context: None,
        },
        Some(Ok(src)) => {
            current.workspace = src.side.workspace.clone();
            session_reuse::decide(Mode::Adjacent, &current, Some(src), threshold)
        }
        _ => session_reuse::decide(Mode::Adjacent, &current, None, threshold),
    };
    let src = source.as_ref().and_then(|s| s.as_ref().ok());
    match plan {
        Plan::Resume {
            source_stage,
            source_run,
            context,
            ..
        } => json!({
            "kind": "reused", "stage": source_stage, "run": source_run,
            "stage_name": src.map(|s| s.stage_name.clone()), "context": context, "threshold": threshold,
        }),
        Plan::Fresh {
            reason: Reason::ClientNotReusable | Reason::NoPrior,
            ..
        } => Value::Null,
        Plan::Fresh { reason, context } => json!({
            "kind": "fresh", "reason": reason.as_str(),
            "stage_name": src.map(|s| s.stage_name.clone()),
            "context": context, "threshold": context.map(|_| threshold),
            "prior": src.map_or(Value::Null, |s| prior_value(reason, &current, &s.side)),
        }),
    }
}

/// A Codex / Claude run started with the minimal launch (`launch.diet` in evidence).
fn diet(ev: &Value) -> bool {
    !ev["launch"]["diet"].is_null()
}

/// Rounded-up suggestion for a stage budget from runs after the diet (R9): median × 1.5, in
/// steps of 10K.
fn suggested_budget(totals: &[u64]) -> Value {
    let Some(median) = median(totals) else {
        return Value::Null;
    };
    let value = (median.saturating_mul(3) / 2).div_ceil(10_000) * 10_000;
    json!({"value": value, "runs": totals.len(), "median": median})
}

fn median(values: &[u64]) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    let mut v = values.to_vec();
    v.sort_unstable();
    let m = v.len() / 2;
    Some(if v.len() % 2 == 1 {
        v[m]
    } else {
        (v[m - 1] + v[m]).div_ceil(2)
    })
}

/// Totals of passed runs after the diet, by stage name, across the project.
fn diet_totals(home: &Path, p: &Project) -> std::collections::BTreeMap<String, Vec<u64>> {
    let mut out: std::collections::BTreeMap<String, Vec<u64>> = Default::default();
    for w in &p.work {
        for a in &w.attempts {
            let dir = workflow::attempt_dir(home, &p.id, &w.id, &a.id);
            let ev = read_evidence(&dir);
            if !diet(&ev) || ev["status"] != "succeeded" || ev["mode"] == "external" {
                continue;
            }
            let total = usage::summary(&dir).tokens.total();
            if total == 0 {
                continue;
            }
            let name = w.stages().get(a.stage).map_or("?", |s| s.name.as_str());
            out.entry(name.to_owned()).or_default().push(total);
        }
    }
    out
}

/// One side (before or after the diet) of a comparison row. Runs without a first-call
/// context (interrupted, E12) are counted but left out of the median and averages.
#[derive(Default)]
struct CompareSide {
    runs: usize,
    first: Vec<u64>,
    calls: Vec<u64>,
    totals: Vec<u64>,
}
impl CompareSide {
    fn add(&mut self, s: &usage::Summary) {
        self.runs += 1;
        if let Some(f) = s.first_context {
            self.first.push(f);
            self.calls.push(s.calls as u64);
            self.totals.push(s.tokens.total());
        }
    }
    fn avg(v: &[u64]) -> Option<f64> {
        (!v.is_empty()).then(|| v.iter().sum::<u64>() as f64 / v.len() as f64)
    }
    fn json(&self) -> Value {
        json!({
            "runs": self.runs,
            "first_context_median": median(&self.first),
            "avg_calls": Self::avg(&self.calls),
            "avg_total": Self::avg(&self.totals),
        })
    }
}

#[derive(Default)]
struct Compare(Vec<(String, CompareSide, CompareSide)>);
impl Compare {
    fn add(&mut self, key: &str, after: bool, s: &usage::Summary) {
        let i = self
            .0
            .iter()
            .position(|(k, ..)| k == key)
            .unwrap_or_else(|| {
                self.0
                    .push((key.to_owned(), Default::default(), Default::default()));
                self.0.len() - 1
            });
        let row = &mut self.0[i];
        if after { row.2.add(s) } else { row.1.add(s) }
    }
    /// Rows by average total before the diet, the most expensive first; `change` compares the
    /// average totals and is null while a side has no sample.
    fn rows(mut self) -> Vec<Value> {
        let before =
            |r: &(String, CompareSide, CompareSide)| CompareSide::avg(&r.1.totals).unwrap_or(0.0);
        self.0.sort_by(|a, b| before(b).total_cmp(&before(a)));
        self.0
            .iter()
            .map(|(key, b, a)| {
                let change = match (CompareSide::avg(&b.totals), CompareSide::avg(&a.totals)) {
                    (Some(b), Some(a)) if b > 0.0 => Some(a / b - 1.0),
                    _ => None,
                };
                json!({"key": key, "before": b.json(), "after": a.json(), "change": change})
            })
            .collect()
    }
}

/// "150k", "1.5M", "150000"; empty = None.
fn parse_amount(text: &str) -> Result<Option<u64>> {
    let t: String = text
        .trim()
        .chars()
        .filter(|c| !matches!(c, ',' | '_' | ' '))
        .collect::<String>()
        .to_lowercase();
    if t.is_empty() {
        return Ok(None);
    }
    let (num, mul) = match t.strip_suffix('k') {
        Some(n) => (n, 1e3),
        None => match t.strip_suffix('m') {
            Some(n) => (n, 1e6),
            None => (t.as_str(), 1.0),
        },
    };
    let n: f64 = num
        .parse()
        .ok()
        .filter(|n: &f64| n.is_finite() && *n >= 0.0)
        .with_context(|| format!("无法识别的 Token 数：{}", text.trim()))?;
    Ok(Some((n * mul).round() as u64))
}

/// The project's resume limit (R2): empty restores the default, below the minimum is refused.
pub fn set_resume_limit(home: &Path, pid: &str, tokens: &str) -> Result<()> {
    let limit = parse_amount(tokens)?;
    if let Some(n) = limit {
        ensure!(
            n >= session_reuse::MIN_LIMIT,
            "续接上限至少 {}K",
            session_reuse::MIN_LIMIT / 1000
        );
    }
    change(home, |lib| {
        find_mut(lib, pid)?.resume_limit = limit;
        Ok(())
    })
}

#[cfg(test)]
mod diet_tests {
    use super::*;

    #[test]
    fn amounts_parse_with_units() {
        assert_eq!(parse_amount("150k").unwrap(), Some(150_000));
        assert_eq!(parse_amount(" 1.5M ").unwrap(), Some(1_500_000));
        assert_eq!(parse_amount("120,000").unwrap(), Some(120_000));
        assert_eq!(parse_amount("").unwrap(), None);
        assert!(parse_amount("abc").is_err());
        assert!(parse_amount("-5k").is_err());
    }

    #[test]
    fn suggestion_is_median_times_one_and_a_half_rounded_up() {
        let v = suggested_budget(&[1_000_000, 1_200_000, 5_000_000]);
        assert_eq!(v["median"], 1_200_000);
        assert_eq!(v["value"], 1_800_000);
        assert_eq!(v["runs"], 3);
        assert_eq!(suggested_budget(&[101_000])["value"], 160_000);
        assert!(suggested_budget(&[]).is_null());
        assert_eq!(median(&[1, 4]), Some(3));
    }
}
