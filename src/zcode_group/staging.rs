//! Only AEP's per-run implementation checkout can own a disposable desktop project.
use super::*;
use rusqlite::OptionalExtension;

const RECEIPT: &str = "desktop-source-project.json";

fn owned_checkout(dir: &Path, workspace: &str, repository: &Path) -> Result<bool> {
    let cwd = Path::new(workspace);
    if dir.file_name().and_then(|s| s.to_str()) != Some("zcode-edit")
        || !cwd
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.starts_with("aep-work-"))
    {
        return Ok(false);
    }
    let run = dir.parent().context("missing run")?;
    let ws = Workspace::open(repository)?;
    if cwd == ws.root || run.parent() != Some(ws.data.join("runs").as_path()) {
        return Ok(false);
    }
    let record = read(&run.join("evidence.json"))?;
    let saved = record["desktop_recovery"]["workspace"]
        .as_str()
        .or(record["worktree"].as_str());
    // macOS spells the same temp directory /var and /private/var. The old
    // checkout itself can already be gone; canonicalize its parent only.
    let Some(saved) = saved.map(Path::new) else {
        return Ok(false);
    };
    Ok(record["implementation_source"] == "zcode"
        && record["run_id"].as_str() == run.file_name().and_then(|s| s.to_str())
        && saved.file_name() == cwd.file_name()
        && saved
            .parent()
            .and_then(|p| p.canonicalize().ok())
            .as_deref()
            == cwd.parent()
        && cwd.parent() == Some(std::env::temp_dir().canonicalize()?.as_path()))
}

/// Register the single-root source without opening the desktop's new-task composer.
/// Its usual open-folder flow can reserve an unrelated, untitled task.
pub fn prepare(dir: &Path, job: &Value, repo: &Path) -> Result<Option<String>> {
    let cwd = job["workspace"].as_str().context("missing workspace")?;
    if !owned_checkout(dir, cwd, repo)? {
        return Ok(None);
    }
    let mut db = index()?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(id) = crate::zcode_desktop_send::project_id(&tx, cwd)? {
        if !dir.join(RECEIPT).try_exists()? {
            aep::project::save_json(
                dir,
                RECEIPT,
                &json!({"project":id,"workspace":cwd,"requestId":job["requestId"],"created":false}),
            )?;
        }
        return Ok(Some(id));
    }
    schema_supported(&tx)?;
    let id = uuid::Uuid::new_v4().to_string();
    let name = Path::new(cwd)
        .file_name()
        .and_then(|s| s.to_str())
        .context("invalid checkout")?;
    let now = chrono::Utc::now().timestamp_millis();
    tx.execute("INSERT INTO projects(project_id,host_id,name,position,created_at,updated_at)
        VALUES(?1,'local',?2,(SELECT coalesce(min(position),1)-1 FROM projects WHERE host_id='local'),?3,?3)",
        params![id,name,now])?;
    tx.execute(
        "INSERT INTO project_roots(project_id,root_path,position,is_primary) VALUES(?1,?2,0,1)",
        params![id, cwd],
    )?;
    aep::project::save_json(
        dir,
        RECEIPT,
        &json!({"project":id,"workspace":cwd,"requestId":job["requestId"],"created":true}),
    )?;
    tx.commit()?;
    Ok(Some(id))
}

fn retire(tx: &rusqlite::Transaction<'_>, source: &str, cwd: &str, since: i64) -> Result<bool> {
    let name = Path::new(cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .context("invalid workspace")?;
    let changed = tx.execute(
        "UPDATE projects SET deleted=1,deleted_at=?4,updated_at=?4
        WHERE project_id=?1 AND host_id='local' AND deleted=0 AND name=?2 AND name_overridden=0 AND pinned=0
        AND created_at>=?5 AND EXISTS(SELECT 1 FROM project_roots WHERE project_id=?1 AND root_path=?3 AND is_primary=1)
        AND NOT EXISTS(SELECT 1 FROM project_roots WHERE project_id=?1 AND root_path<>?3)
        AND NOT EXISTS(SELECT 1 FROM tasks WHERE project_id=?1)
        AND NOT EXISTS(SELECT 1 FROM automations WHERE project_id=?1)
        AND NOT EXISTS(SELECT 1 FROM off_peak_tasks WHERE project_id=?1)",
        params![source, name, cwd, chrono::Utc::now().timestamp_millis(), since],
    )?;
    Ok(changed == 1)
}

pub fn cleanup(dir: &Path, repo: &Path) -> Result<()> {
    if !dir.join("desktop-request.json").try_exists()? {
        return Ok(());
    }
    let job = read(&dir.join("desktop-request.json"))?;
    let cwd = job["workspace"].as_str().context("missing workspace")?;
    if !owned_checkout(dir, cwd, repo)? {
        return Ok(());
    }
    let mut db = index()?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let source = if dir.join(RECEIPT).try_exists()? {
        let saved = read(&dir.join(RECEIPT))?;
        if saved["created"] != true
            || saved["workspace"] != cwd
            || saved["requestId"] != job["requestId"]
        {
            return Ok(());
        }
        if !dir.join("desktop-create.json").try_exists()? {
            let rejected = read(&dir.join("desktop-declined.json"))?;
            if rejected["requestId"] != job["requestId"] || rejected["project"] != saved["project"]
            {
                return Ok(());
            }
        }
        saved["project"]
            .as_str()
            .context("missing source project")?
            .to_owned()
    } else if dir.join("desktop-create.json").try_exists()? {
        // Old runs have no provisioning receipt. Require their native creation
        // command as well as the AEP-owned path, project creation time and ID.
        let created = read(&dir.join("desktop-create.json"))?;
        if created["requestId"] != job["requestId"] || created["session"]["workspacePath"] != cwd {
            return Ok(());
        }
        let confirmed: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM session_creation_commands
            WHERE workspace_key=?1 AND command_id=?2 AND session_id=?3 AND client_id='local:client:extension:aep-studio@aep-local')",
            params![cwd, created["operation"]["commandId"].as_str(), created["session"]["sessionId"].as_str()],
            |r| r.get(0),
        )?;
        if !confirmed {
            return Ok(());
        }
        created["session"]["project"]["projectId"]
            .as_str()
            .context("missing source project")?
            .to_owned()
    } else {
        return Ok(());
    };
    let active: Option<bool> = tx
        .query_row(
            "SELECT deleted=0 FROM projects WHERE project_id=?1",
            [&source],
            |r| r.get(0),
        )
        .optional()?;
    if active != Some(true) {
        return Ok(());
    }
    schema_supported(&tx)?;
    let retired = retire(
        &tx,
        &source,
        cwd,
        job["since"].as_i64().context("missing dispatch time")?,
    )?;
    tx.commit()?;
    if retired {
        aep::project::save_json(
            dir,
            "desktop-cleanup.json",
            &json!({"project":source,"workspace":cwd,"empty":true,"action":"hide-empty-project"}),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retire_only_an_empty_unmodified_single_root_local_project() -> Result<()> {
        let mut db = Connection::open_in_memory()?;
        db.execute_batch("CREATE TABLE projects(project_id,host_id,name,created_at,updated_at,deleted_at,deleted,pinned,name_overridden);
            INSERT INTO projects VALUES('source','local','aep-work-sample',10,10,NULL,0,0,0);
            CREATE TABLE project_roots(project_id,root_path,is_primary);
            INSERT INTO project_roots VALUES('source','/tmp/aep-work-sample',1);
            CREATE TABLE tasks(project_id,archived,deleted,title);
            CREATE TABLE automations(project_id);
            CREATE TABLE off_peak_tasks(project_id);")?;
        let cwd = "/tmp/aep-work-sample";
        for mutation in [
            "INSERT INTO tasks VALUES('source',0,0,'')",
            "INSERT INTO tasks VALUES('source',1,0,'archived')",
            "INSERT INTO tasks VALUES('source',0,1,'deleted')",
            "INSERT INTO automations VALUES('source')",
            "INSERT INTO off_peak_tasks VALUES('source')",
            "UPDATE projects SET pinned=1",
            "UPDATE projects SET name_overridden=1",
            "UPDATE projects SET host_id='remote'",
            "UPDATE projects SET created_at=4",
            "UPDATE projects SET name='renamed'",
            "INSERT INTO project_roots VALUES('source','/elsewhere',0)",
        ] {
            let tx = db.transaction()?;
            tx.execute(mutation, [])?;
            assert!(!retire(&tx, "source", cwd, 5)?, "{mutation}");
            tx.rollback()?;
        }
        let tx = db.transaction()?;
        assert!(!retire(&tx, "another", cwd, 5)?);
        assert!(!retire(&tx, "source", "/other/aep-work-sample", 5)?);
        assert!(retire(&tx, "source", cwd, 5)?);
        assert!(!retire(&tx, "source", cwd, 5)?);
        assert_eq!(
            tx.query_row("SELECT root_path FROM project_roots", [], |r| r
                .get::<_, String>(0))?,
            cwd
        );
        tx.commit()?;
        Ok(())
    }
}
