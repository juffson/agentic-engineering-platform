//! Read desktop membership from the local index and execution details from AEP receipts.
use super::*;
use rusqlite::OptionalExtension;

fn optional(dir: &Path, name: &str) -> Result<Value> {
    let path = dir.join(name);
    if path.try_exists()? {
        read(&path)
    } else {
        Ok(Value::Null)
    }
}

fn row(db: &Connection, dir: &Path, repo: &Path) -> Result<Value> {
    let request = optional(dir, "desktop-request.json")?;
    let created = optional(dir, "desktop-create.json")?;
    let source = optional(dir, "desktop-source-project.json")?;
    let cleaned = optional(dir, "desktop-cleanup.json")?;
    let legacy = optional(dir, "session.json")?;
    let task = optional(dir, "desktop-task.json")?;
    let session = task["session"]
        .as_str()
        .or(created["session"]["sessionId"].as_str())
        .or_else(|| {
            (legacy["transport"] == "desktop")
                .then(|| legacy["session"].as_str())
                .flatten()
        });
    let workspace = task["workspace"]
        .as_str()
        .or(request["workspace"].as_str())
        .or(legacy["workspace"].as_str())
        .context("缺少工作区回执")?;
    let open_ref = if !created.is_null() {
        &created["session"]
    } else {
        &legacy
    };
    let open_id = open_ref["sessionId"]
        .as_str()
        .or(open_ref["session"].as_str());
    let open_workspace = open_ref["workspacePath"]
        .as_str()
        .or(open_ref["workspace"].as_str());
    ensure!(
        open_id.is_none() || (open_id == session && open_workspace == Some(workspace)),
        "会话和工作区回执不匹配"
    );
    let current = db.query_row(
        "SELECT p.project_id, p.name, t.title FROM tasks t JOIN projects p ON p.project_id=t.project_id
         WHERE t.host_id='local' AND p.host_id='local' AND t.task_id=?1 AND t.root_path=?2
         AND p.deleted=0 AND t.deleted=0 AND coalesce(t.cli_deleted,0)=0",
        params![session, workspace], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))
    ).optional()?;
    let target = crate::zcode_desktop_send::project_id(db, repo.to_str().context("无效仓库路径")?)?;
    let source_id = source["project"]
        .as_str()
        .or(created["session"]["project"]["projectId"].as_str());
    let confirmed_cleanup = source_id.is_some()
        && cleaned["project"].as_str() == source_id
        && cleaned["workspace"].as_str() == Some(workspace)
        && cleaned["empty"] == true;
    let isolated = Path::new(workspace) != repo;
    let cleanup = if !isolated {
        "使用仓库主目录".into()
    } else if let Some(id) = source_id {
        let state = db.query_row(
            "SELECT deleted, (SELECT count(*) FROM tasks WHERE project_id=?1) FROM projects WHERE project_id=?1 AND host_id='local'",
            [id], |r| Ok((r.get::<_,bool>(0)?, r.get::<_,i64>(1)?))
        ).optional()?;
        match state {
            Some((true, _)) if confirmed_cleanup => "空项目已清理".into(),
            Some((true, _)) => "原项目已从侧栏移除".into(),
            Some((false, n)) if n > 0 => format!("保留原项目 · {n} 条任务记录"),
            Some((false, _)) if source["created"] == false => "保留已有项目".into(),
            Some((false, _)) => "原项目仍在侧栏 · 同步时核对清理条件".into(),
            None => "未找到原项目".into(),
        }
    } else {
        "无可核对的临时项目回执".into()
    };
    let role = match dir.file_name().and_then(|s| s.to_str()) {
        Some("zcode-edit") => "实现",
        Some("zcode-plan") => "审查",
        _ => "工作流阶段",
    };
    let run = if role == "工作流阶段" {
        dir
    } else {
        dir.parent().context("缺少运行目录")?
    };
    let title = current
        .as_ref()
        .map(|c| c.2.as_str())
        .filter(|s| !s.trim().is_empty());
    Ok(
        json!({"dir":dir,"run":run.file_name().and_then(|n|n.to_str()),"role":role,"session":session,
        "title":title,"workspace":workspace,"isolated":isolated,"workspace_exists":Path::new(workspace).is_dir(),
        "project":current.as_ref().map(|c|&c.0),"project_name":current.as_ref().map(|c|&c.1),
        "grouped":current.as_ref().is_some_and(|c|Some(&c.0)==target.as_ref()),
        "cleanup":cleanup,"source_project":source_id,"cleaned":confirmed_cleanup,
        "can_open":open_id.is_some() && current.is_some() && (!created.is_null() || legacy["transport"]=="desktop"),"since":request["since"].as_i64().unwrap_or_default()}),
    )
}

pub fn list(repo: &Path, scope: Option<(&Path, &str)>) -> Result<Vec<Value>> {
    let ws = Workspace::open(repo)?;
    let repo = ws.root.canonicalize()?;
    let dirs: Vec<_> = evidence_dirs(&ws, scope)?
        .into_iter()
        .filter(|dir| {
            [
                "desktop-task.json",
                "desktop-create.json",
                "desktop-source-project.json",
            ]
            .iter()
            .any(|n| dir.join(n).is_file())
                || (dir.join("desktop-request.json").is_file()
                    && dir.join("session.json").is_file())
        })
        .collect();
    if dirs.is_empty() {
        return Ok(vec![]);
    }
    let mut db = index_with(OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let tx = db.transaction()?;
    let mut rows: Vec<_> = dirs
        .iter()
        .map(|dir| {
            row(&tx, dir, &repo).unwrap_or_else(|e| json!({"dir":dir,"error":format!("{e:#}")}))
        })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r["since"].as_i64().unwrap_or_default()));
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aep::project::save_json;

    #[test]
    fn history_uses_current_membership_and_distinguishes_cleanup_receipts() -> Result<()> {
        let db = Connection::open_in_memory()?;
        db.execute_batch("CREATE TABLE hosts(host_id,kind); INSERT INTO hosts VALUES('local','local');
          CREATE TABLE projects(project_id,name,host_id,deleted);
          INSERT INTO projects VALUES('target','My project','local',0),('source','aep-work-x','local',0);
          CREATE TABLE project_roots(project_id,root_path,is_primary);
          INSERT INTO project_roots VALUES('target','/repo',1),('source','/tmp/aep-work-x',1);
          CREATE TABLE tasks(task_id,project_id,host_id,root_path,deleted,cli_deleted,title);
          INSERT INTO tasks VALUES('task','source','local','/tmp/aep-work-x',0,0,'Implement'),
                                 ('draft','source','local','/tmp/aep-work-x',0,0,'');")?;
        let temp = tempfile::tempdir()?;
        let dir = temp.path();
        let path = "/tmp/aep-work-x";
        let repo = Path::new("/repo");
        save_json(dir, "desktop-request.json", &json!({"workspace":path}))?;
        save_json(
            dir,
            "session.json",
            &json!({"session":"task","workspace":path,"transport":"desktop"}),
        )?;
        save_json(
            dir,
            "desktop-source-project.json",
            &json!({"project":"source","created":true}),
        )?;
        let original = row(&db, dir, repo)?;
        assert_eq!(original["grouped"], false);
        assert_eq!(original["can_open"], true);
        assert_eq!(original["project_name"], "aep-work-x");
        db.execute(
            "UPDATE tasks SET project_id='target' WHERE task_id='task'",
            [],
        )?;
        let grouped = row(&db, dir, repo)?;
        assert_eq!(grouped["grouped"], true);
        assert_eq!(grouped["workspace"], path);
        assert_eq!(grouped["cleanup"], "保留原项目 · 1 条任务记录");
        db.execute(
            "UPDATE projects SET deleted=1 WHERE project_id='source'",
            [],
        )?;
        assert_eq!(row(&db, dir, repo)?["cleanup"], "原项目已从侧栏移除");
        save_json(
            dir,
            "desktop-cleanup.json",
            &json!({"project":"source","workspace":path,"empty":true}),
        )?;
        assert_eq!(row(&db, dir, repo)?["cleanup"], "空项目已清理");
        save_json(
            dir,
            "desktop-task.json",
            &json!({"session":"different","workspace":path}),
        )?;
        assert!(row(&db, dir, repo).is_err());
        Ok(())
    }
}
