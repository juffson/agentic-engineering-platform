//! Exchange one task with the user's ZCode desktop composer. Conversation history is read-only.
use aep::project::save_json;
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::{Value, json};
use std::{fs, path::Path, time::Duration};

fn readonly(path: &Path) -> Result<Connection> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("打开 ZCode 桌面记录 {}", path.display()))?;
    db.busy_timeout(Duration::from_secs(2))?;
    Ok(db)
}

struct Reply {
    evidence: Value,
    text: String,
}

pub(super) fn find_message(
    index: &Connection,
    db: &Connection,
    workspace: &str,
    since: i64,
    dispatch: &str,
) -> Result<Option<(String, String)>> {
    let mut tasks = index.prepare("SELECT task_id FROM tasks WHERE deleted=0 AND COALESCE(root_path,workspace_path)=?1 AND updated_at>=?2")?;
    let mut found = None;
    let mut query = db.prepare(
        "SELECT DISTINCT m.id FROM message m JOIN part p ON p.message_id=m.id
            WHERE m.session_id=?1 AND m.time_created>=?2 AND json_extract(m.data,'$.role')='user'
            AND json_extract(p.data,'$.type')='text' AND trim(json_extract(p.data,'$.text'))=?3",
    )?;
    for task in tasks.query_map(params![workspace, since], |r| r.get::<_, String>(0))? {
        let task = task?;
        for message in query.query_map(params![task, since, dispatch], |r| r.get::<_, String>(0))? {
            ensure!(found.is_none(), "同一派发已发送多次，请重新发起此轮");
            found = Some((task.clone(), message?));
        }
    }
    Ok(found)
}

fn read_reply(
    db: &Connection,
    workspace: &str,
    task: &str,
    message: &str,
    model: &Value,
) -> Result<Option<Reply>> {
    let turn: Option<(String, String, Option<String>)> = db.query_row(
        "SELECT turn_id,status,error_code FROM turn_usage WHERE session_id=?1 AND user_message_id=?2",
        params![task, message],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).optional()?;
    let Some((turn, status, error)) = turn else {
        return Ok(None);
    };
    match status.as_str() {
        "running" => return Ok(None),
        "completed" => (),
        _ => bail!(
            "ZCode 桌面任务 {task} {status} · {}",
            error.as_deref().unwrap_or("请查看桌面任务")
        ),
    }
    let mut usage = db.prepare(
        "SELECT provider_id,model_id,variant,assistant_message_id FROM model_usage
        WHERE session_id=?1 AND turn_id=?2 AND query_source='main_turn' AND status='completed'
        ORDER BY completed_at, id",
    )?;
    let mut last = None;
    for row in usage.query_map(params![task, turn], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
        ))
    })? {
        let (provider, name, variant, id) = row?;
        ensure!(
            model["providerId"] == provider
                && model["modelId"] == name
                && model
                    .pointer("/options/reasoningLevel")
                    .and_then(Value::as_str)
                    .is_none_or(|v| Some(v) == variant.as_deref()),
            "桌面任务 {task} 实际使用 {provider}/{name}@{}，与本轮选择 {model} 不符",
            variant.as_deref().unwrap_or("默认")
        );
        last = Some((provider, name, variant, id));
    }
    let (provider, name, variant, id) = last.context("缺少 ZCode 主对话模型用量回执")?;
    let id = id.context("缺少 ZCode 助手消息 ID")?;
    let mut parts = db.prepare(
        "SELECT json_extract(data,'$.text') FROM part WHERE session_id=?1 AND message_id=?2
        AND json_extract(data,'$.type')='text' ORDER BY sequence,id",
    )?;
    let text = parts
        .query_map(params![task, id], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?
        .join("\n");
    ensure!(!text.trim().is_empty(), "桌面任务 {task} 缺少最终交接答复");
    Ok(Some(Reply {
        evidence: json!({"transport":"desktop","client":"ZCode","session":task,"turn":turn,"message":id,
        "providerId":provider,"modelId":name,"reasoningLevel":variant,"workspace":workspace}),
        text,
    }))
}

/// Collect an already completed, pinned desktop input without creating a session or sending a prompt.
pub fn completed(dir: &Path, workspace: &Path, selected: &str) -> Result<String> {
    let task = aep::workspace::read(&dir.join("desktop-task.json"))?;
    let job = aep::workspace::read(&dir.join("desktop-request.json"))?;
    let creation = aep::workspace::read(&dir.join("desktop-create.json"))?;
    let workspace = workspace
        .to_str()
        .context("invalid desktop worktree path")?;
    let session = task["session"]
        .as_str()
        .context("missing desktop session")?;
    let input = task["userMessage"]
        .as_str()
        .context("missing desktop message")?;
    let model = &job["model"];
    let expected = aep::zcode_catalog::effective_selection(selected);
    ensure!(
        task["workspace"] == workspace
            && job["workspace"] == workspace
            && creation["session"]["workspacePath"] == workspace
            && creation["session"]["sessionId"] == session
            && creation["requestId"] == job["requestId"]
            && job["mode"] == "edit"
            && format!(
                "{}/{}",
                model["providerId"].as_str().unwrap_or(""),
                model["modelId"].as_str().unwrap_or("")
            ) == expected,
        "saved desktop task does not match the original workspace and model"
    );
    let home = std::env::home_dir()
        .context("missing home directory")?
        .join(".zcode");
    let index = readonly(&home.join("v2/tasks-index.sqlite"))?;
    let db = readonly(&home.join("cli/db/db.sqlite"))?;
    let dispatched = find_message(
        &index,
        &db,
        workspace,
        job["since"].as_i64().context("missing dispatch time")?,
        job["dispatch"].as_str().context("missing dispatch text")?,
    )?;
    ensure!(
        dispatched
            .as_ref()
            .is_some_and(|(s, m)| s == session && m == input),
        "original desktop input does not match"
    );
    let reply = read_reply(&db, workspace, session, input, model)?
        .context("original desktop turn has not completed")?;
    save_json(dir, "session.json", &reply.evidence)?;
    save_json(dir, "zcode-model.json", &reply.evidence)?;
    Ok(reply.text)
}

pub fn converse(
    dir: &Path,
    workspace: &Path,
    mode: &str,
    model: &Value,
    prompt: &str,
    repository: Option<&Path>,
) -> Result<String> {
    let home = std::env::home_dir()
        .context("缺少 ZCode 用户目录")?
        .join(".zcode");
    let index = readonly(&home.join("v2/tasks-index.sqlite"))?;
    let db = readonly(&home.join("cli/db/db.sqlite"))?;
    let workspace = workspace.canonicalize()?;
    let workspace = workspace.to_str().context("工作区路径必须为 UTF-8")?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("desktop-dispatch.lock"))?;
    lock.lock()?;
    let marker = uuid::Uuid::new_v4().simple().to_string();
    let role = match mode {
        "edit" => "实现",
        "guarded" => "验收",
        _ => "审查",
    };
    let prompt_file = dir.join("desktop-prompt.txt");

    let name = dir.file_name().and_then(|s| s.to_str()).unwrap_or("AEP");
    let run = if name.starts_with("zcode-") {
        dir.parent()
            .and_then(Path::file_name)
            .and_then(|s| s.to_str())
            .unwrap_or(name)
    } else {
        name
    };
    let instruction = if mode == "plan" {
        "本轮只读取给定材料并返回审查意见，请勿修改项目文件。\n"
    } else {
        ""
    };
    let dispatch = format!(
        "AEP {role} · {run} · {marker}\n{instruction}\n{}",
        prompt.trim_end()
    );
    let request = dir.join("desktop-request.json");
    let job = if request.try_exists()? {
        let saved = aep::workspace::read(&request)?;
        ensure!(
            saved["model"] == *model
                && saved["mode"] == mode
                && saved["workspace"] == workspace
                && fs::read_to_string(&prompt_file)? == prompt,
            "本轮已有不同的 ZCode 派发请求"
        );
        saved
    } else {
        fs::write(&prompt_file, prompt)?;
        let job = json!({"requestId":marker,"workspace":workspace,"dispatch":dispatch,"model":model,"mode":mode,
            "since":chrono::Utc::now().timestamp_millis()});
        save_json(dir, "desktop-request.json", &job)?;
        job
    };
    let (task, message) = crate::zcode_desktop_send::dispatch(
        &index,
        &db,
        dir,
        &job,
        job["since"].as_i64().unwrap_or(0),
    )?;
    save_json(
        dir,
        "desktop-task.json",
        &json!({"session":task,"userMessage":message,"workspace":workspace}),
    )?;
    if let Some(repo) = repository
        && let Err(e) = crate::zcode_group::group(dir, repo)
    {
        eprintln!("ZCode 项目归组失败，可用 aep zcode-group 重试：{e:#}");
        save_json(
            dir,
            "desktop-group.json",
            &json!({"session":task,"error":format!("{e:#}")}),
        )?;
    }
    let _approval_monitor = crate::zcode_desktop_send::monitor(dir, &task);
    loop {
        if let Some(reply) = read_reply(&db, workspace, &task, &message, model)? {
            save_json(dir, "session.json", &reply.evidence)?;
            save_json(dir, "zcode-model.json", &reply.evidence)?;
            return Ok(reply.text);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_uses_the_addressed_desktop_task_turn_and_verified_model() -> Result<()> {
        let db = Connection::open_in_memory()?;
        db.execute_batch(r#"
          CREATE TABLE tasks(task_id,deleted,root_path,workspace_path,updated_at);
          INSERT INTO tasks VALUES ('s',0,'/checkout','/project',2),('other',0,'/other','/other',2);
          CREATE TABLE message(id,session_id,time_created,data);
          CREATE TABLE part(id,session_id,message_id,sequence,data);
          INSERT INTO message VALUES ('user','s',2,'{"role":"user"}');
          INSERT INTO part VALUES (1,'s','user',0,'{"type":"text","text":"unique dispatch"}');
          CREATE TABLE turn_usage(turn_id,status,error_code,session_id,user_message_id);
          INSERT INTO turn_usage VALUES ('t','running',NULL,'s','user');
          CREATE TABLE model_usage(id,session_id,turn_id,query_source,status,completed_at,provider_id,model_id,variant,assistant_message_id);
          INSERT INTO model_usage VALUES
            (1,'s','t','title','completed',3,'other','ignored',NULL,'title'),
            (2,'s','t','main_turn','completed',4,'plan','glm','max','reply');
          INSERT INTO part VALUES
            (2,'s','reply',1,'{"type":"text","text":"handoff"}'),
            (3,'s','reply',0,'{"type":"reasoning","text":"internal"}'),
            (4,'other','reply',1,'{"type":"text","text":"wrong session"}');
        "#)?;
        let model = json!({"providerId":"plan","modelId":"glm","options":{"reasoningLevel":"max"}});
        assert_eq!(
            find_message(&db, &db, "/checkout", 1, "unique dispatch")?,
            Some(("s".into(), "user".into()))
        );
        assert!(find_message(&db, &db, "/project", 1, "unique dispatch")?.is_none());
        assert!(find_message(&db, &db, "/checkout", 3, "unique dispatch")?.is_none());
        assert!(read_reply(&db, "/checkout", "s", "user", &model)?.is_none());
        db.execute("UPDATE turn_usage SET status='completed'", [])?;
        let default = json!({"providerId":"plan","modelId":"glm"});
        let actual = read_reply(&db, "/checkout", "s", "user", &default)?.unwrap();
        assert_eq!(actual.evidence["reasoningLevel"], "max");
        let reply = read_reply(&db, "/checkout", "s", "user", &model)?.unwrap();
        assert_eq!(reply.text, "handoff");
        assert_eq!(reply.evidence["turn"], "t");
        db.execute("UPDATE model_usage SET variant='low' WHERE id=2", [])?;
        assert!(read_reply(&db, "/checkout", "s", "user", &model).is_err());
        db.execute(
            "UPDATE model_usage SET variant='max',provider_id='another plan' WHERE id=2",
            [],
        )?;
        assert!(read_reply(&db, "/checkout", "s", "user", &model).is_err());
        Ok(())
    }
}
