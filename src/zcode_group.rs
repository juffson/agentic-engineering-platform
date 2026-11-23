//! Local desktop project membership. The extension creates and operates the session.
//! The worktree-aware project index stores sidebar ownership separately from the execution directory.
use aep::{
    project::directory::Binding,
    workspace::{Workspace, read},
};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

pub mod history;
pub mod staging;

fn index_with(flags: OpenFlags) -> Result<Connection> {
    let file = std::env::home_dir()
        .context("missing home")?
        .join(".zcode/v2/tasks-index.sqlite");
    let db = Connection::open_with_flags(file, flags)?;
    db.busy_timeout(Duration::from_secs(5))?;
    Ok(db)
}

fn index() -> Result<Connection> {
    index_with(OpenFlags::SQLITE_OPEN_READ_WRITE)
}

fn schema_supported(tx: &Connection) -> Result<()> {
    let supported: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM schema_migration WHERE id='0010_task_worktree_root'
            AND checksum='3aecdd4658f4672147d1e757b6a401bb986daf6590838a8072cb3409a4395d79') AND NOT EXISTS
            (SELECT 1 FROM schema_migration WHERE id > '0010_task_worktree_root')", [], |r| r.get(0))?;
    ensure!(supported, "当前 ZCode 项目索引版本尚未验证");
    Ok(())
}

fn assign(db: &mut Connection, dir: &Path, repository: &str) -> Result<Value> {
    let task = if dir.join("desktop-task.json").try_exists()? {
        read(&dir.join("desktop-task.json"))?
    } else {
        let saved = read(&dir.join("session.json"))?;
        ensure!(saved["transport"] == "desktop", "此记录不是桌面会话");
        saved
    };
    let session = task["session"]
        .as_str()
        .context("missing desktop session")?;
    let cwd = task["workspace"]
        .as_str()
        .context("missing desktop workspace")?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let target = crate::zcode_desktop_send::project_id(&tx, repository)?
        .context("ZCode 未登记仓库主目录")?;
    let current: String = tx
        .query_row(
            "SELECT project_id FROM tasks WHERE task_id=?1 AND host_id='local'
        AND deleted=0 AND coalesce(cli_deleted,0)=0 AND root_path=?2",
            params![session, cwd],
            |r| r.get(0),
        )
        .context("找不到回执对应的本机 ZCode 任务")?;
    let changed = current != target;
    if changed {
        let created = read(&dir.join("desktop-create.json"))?;
        let job = read(&dir.join("desktop-request.json"))?;
        let command = created["operation"]["commandId"]
            .as_str()
            .context("missing desktop command")?;
        ensure!(
            created["requestId"].is_string()
                && created["requestId"] == job["requestId"]
                && created["session"]["sessionId"] == session
                && created["session"]["workspacePath"] == cwd
                && created["session"]["project"]["projectId"] == current
                && job["workspace"] == cwd,
            "任务归属已改变或原派发回执不匹配"
        );
        // AEP's automatic source project has only its isolated directory. Never
        // widen execution access by inheriting the destination project's roots.
        schema_supported(&tx)?;
        let count = tx.execute("UPDATE tasks SET project_id=?1, handoff_pending=handoff_pending | 8,
            execution_directories=coalesce(execution_directories,'[]')
            WHERE task_id=?2 AND project_id=?3 AND workspace_path=?4 AND root_path=?4 AND worktree_root IS NULL
            AND host_id='local' AND deleted=0 AND coalesce(cli_deleted,0)=0
            AND EXISTS(SELECT 1 FROM session_creation_commands c WHERE c.session_id=tasks.task_id
                AND c.workspace_key=tasks.workspace_key AND c.command_id=?5 AND c.client_id='local:client:extension:aep-studio@aep-local')
            AND EXISTS(SELECT 1 FROM project_roots WHERE project_id=?3 AND root_path=?4 AND is_primary=1)
            AND NOT EXISTS(SELECT 1 FROM project_roots WHERE project_id=?3 AND root_path<>?4)",
            params![target,session,current,cwd,command])?;
        ensure!(count == 1, "无法确认此任务来自 AEP 的独立工作区");
    }
    tx.commit()?;
    Ok(
        json!({"session":session,"workspace":cwd,"project":target,"repository":repository,"moved":changed}),
    )
}

pub fn group(dir: &Path, repository: &Path) -> Result<Value> {
    let repo = repository.canonicalize()?;
    let repo = repo.to_str().context("仓库路径必须为 UTF-8")?;
    let mut db = index()?;
    crate::zcode_desktop_send::local_project(&db, repo)?;
    let result = assign(&mut db, dir, repo)?;
    // Keep the first successful move receipt when a later sync just verifies it.
    let saved = read(&dir.join("desktop-group.json")).ok();
    if !saved.is_some_and(|v| {
        v["session"] == result["session"]
            && v["project"] == result["project"]
            && v["workspace"] == result["workspace"]
            && v["error"].is_null()
    }) {
        aep::project::save_json(dir, "desktop-group.json", &result)?;
    }
    staging::cleanup(dir, repository)?;
    Ok(result)
}

fn subdirs(path: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    if path.try_exists()? {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                dirs.push(entry.path());
            }
        }
    }
    dirs.sort();
    Ok(dirs)
}

fn evidence_dirs(ws: &Workspace, scope: Option<(&Path, &str)>) -> Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    for run in subdirs(&ws.data.join("runs"))? {
        for role in ["zcode-edit", "zcode-plan"] {
            dirs.push(run.join(role));
        }
    }
    let scope = match scope {
        Some((home, id)) => Some((home.to_owned(), id.to_owned())),
        None => Binding::file(&ws.root)
            .map(|f| Binding::read(&f))
            .transpose()?
            .flatten()
            .map(|b| (b.home, b.project)),
    };
    if let Some((home, id)) = scope {
        aep::workspace::ident(&id, 64)?;
        for work in subdirs(&home.join("workflows").join(id))? {
            for run in subdirs(&work.join("runs"))? {
                if run.join("desktop-request.json").is_file()
                    || run.join("desktop-task.json").is_file()
                {
                    let req = read(&run.join("request.json"))?;
                    if req["repository"]
                        .as_str()
                        .and_then(|path| Workspace::open(Path::new(path)).ok())
                        .is_some_and(|r| r.data == ws.data)
                    {
                        dirs.push(run);
                    }
                }
            }
        }
    }
    Ok(dirs)
}

fn sync_one(dir: &Path, repo: &Path) -> Result<Option<Value>> {
    let completed_desktop = dir.join("desktop-request.json").is_file()
        && dir.join("session.json").is_file()
        && read(&dir.join("session.json"))?["transport"] == "desktop";
    if dir.join("desktop-task.json").try_exists()? || completed_desktop {
        return Ok(Some(group(dir, repo)?));
    }
    if dir.join("desktop-declined.json").is_file() {
        // A retry of this request may already be in flight.
        let lock = fs::File::open(dir.join("desktop-dispatch.lock"))?;
        match lock.try_lock() {
            Ok(()) => staging::cleanup(dir, repo)?,
            Err(std::fs::TryLockError::WouldBlock) => (),
            Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
        }
    }
    Ok(None)
}

pub fn sync(repo: &Path, verify: bool, scope: Option<(&Path, &str)>) -> Result<Vec<Value>> {
    let ws = Workspace::open(repo)?;
    let mut outcomes = Vec::new();
    let mut errors = Vec::new();
    for dir in evidence_dirs(&ws, scope)? {
        match sync_one(&dir, &ws.root) {
            Ok(Some(row)) => outcomes.push(row),
            Ok(None) => (),
            Err(e) => errors.push(format!("{} · {e:#}", dir.display())),
        }
    }
    ensure!(errors.is_empty(), "部分记录无法同步：{}", errors.join("\n"));
    if verify && let Some(first) = outcomes.first() {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let project = first["project"].clone();
        let sessions: Vec<_> = outcomes.iter().map(|v| &v["session"]).collect();
        let root = std::env::home_dir()
            .context("missing home")?
            .join(".aep/zcode");
        let reply = crate::zcode_desktop_send::exchange(
            &root,
            &json!({"action":"projectSessions",
            "projectId":project,"sessions":sessions,"requestId":id}),
        )?;
        ensure!(
            reply["requestId"] == id && reply["projectId"] == project,
            "ZCode 核对回执不匹配"
        );
        let listed = reply["sessions"].as_array().context("缺少项目会话列表")?;
        for row in &mut outcomes {
            let reference = listed
                .iter()
                .find(|s| {
                    s["sessionId"] == row["session"]
                        && s["sourceServerId"] == "local"
                        && s["project"]["projectId"] == project
                        && s["workspacePath"] == row["workspace"]
                })
                .with_context(|| format!("ZCode 项目列表缺少 {}", row["session"]))?;
            row["desktopSession"] = reference.clone();
        }
    }
    Ok(outcomes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aep::project::save_json;

    #[test]
    fn group_only_receipted_local_task_and_preserve_execution_scope() -> Result<()> {
        let mut db = Connection::open_in_memory()?;
        db.execute_batch("CREATE TABLE hosts(host_id,kind); INSERT INTO hosts VALUES('local','local');
          CREATE TABLE projects(project_id,host_id,deleted); INSERT INTO projects VALUES('target','local',0),('source','local',0);
          CREATE TABLE project_roots(project_id,root_path,is_primary); INSERT INTO project_roots VALUES('target','/repo',1),('source','/work',1);
          CREATE TABLE tasks(task_id, project_id,host_id,workspace_key,workspace_path,root_path,deleted,cli_deleted,worktree_root,handoff_pending,execution_directories);
          INSERT INTO tasks VALUES('task','source','local','/work','/work','/work',0,0,NULL,2,NULL),('unrelated','source','local','/work','/work','/work',0,0,NULL,2,NULL);
          CREATE TABLE schema_migration(id,checksum);
          INSERT INTO schema_migration VALUES('0010_task_worktree_root','3aecdd4658f4672147d1e757b6a401bb986daf6590838a8072cb3409a4395d79');
          CREATE TABLE session_creation_commands(workspace_key,session_id,command_id,client_id);")?;
        let temp = tempfile::tempdir()?;
        let dir = temp.path();
        save_json(
            dir,
            "desktop-task.json",
            &json!({"session":"task","workspace":"/work"}),
        )?;
        save_json(
            dir,
            "desktop-request.json",
            &json!({"requestId":"nonce","workspace":"/work"}),
        )?;
        save_json(
            dir,
            "desktop-create.json",
            &json!({"requestId":"nonce","operation":{"commandId":"cmd"},"session":{"project":{"projectId":"source"},"sessionId":"task","workspacePath":"/work"}}),
        )?;
        assert!(assign(&mut db, dir, "/repo").is_err());
        db.execute("INSERT INTO session_creation_commands VALUES('/work','task','cmd','local:client:extension:aep-studio@aep-local')",[])?;
        assert_eq!(assign(&mut db, dir, "/repo")?["moved"], true);
        assert_eq!(assign(&mut db, dir, "/repo")?["moved"], false);
        let fields:(String,String,String,i64)=db.query_row("SELECT project_id,root_path,execution_directories,handoff_pending FROM tasks WHERE task_id='task'",[],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        assert_eq!(fields, ("target".into(), "/work".into(), "[]".into(), 10));
        assert_eq!(
            db.query_row(
                "SELECT project_id FROM tasks WHERE task_id='unrelated'",
                [],
                |r| r.get::<_, String>(0)
            )?,
            "source"
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM project_roots", [], |r| r
                .get::<_, i64>(0))?,
            2
        );
        Ok(())
    }
}
