//! Exchange one task with the user's ZCode desktop composer. Conversation history is read-only.
use aep::project::save_json;
use aep::zcode_receipt;
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::{Value, json};
use std::{fs, path::Path, time::Duration};

pub(crate) fn readonly(path: &Path) -> Result<Connection> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("打开 ZCode 桌面记录 {}", path.display()))?;
    db.busy_timeout(Duration::from_secs(2))?;
    Ok(db)
}

struct Reply {
    evidence: Value,
    text: String,
}

pub(crate) struct Usage {
    pub provider: String,
    pub model: String,
    pub variant: Option<String>,
    pub turn: String,
    pub message: Option<String>,
}

pub(crate) fn verify_model(
    model: Option<&Value>,
    selected: &str,
    provider: &str,
    name: &str,
    variant: Option<&str>,
    check_level: bool,
) -> Result<()> {
    if let Some(model) = model {
        ensure!(
            model["providerId"] == provider
                && model["modelId"] == name
                && (!check_level
                    || model
                        .pointer("/options/reasoningLevel")
                        .and_then(Value::as_str)
                        .is_none_or(|level| Some(level) == variant)),
            "{}",
            zcode_receipt::mismatch(selected, provider, name, variant)
        );
    }
    Ok(())
}

pub(crate) fn main_turn_usage(
    db: &Connection,
    session: &str,
    turn: Option<&str>,
    model: Option<&Value>,
    selected: &str,
) -> Result<Option<Usage>> {
    let mut query = db.prepare(
        "SELECT provider_id,model_id,variant,turn_id,assistant_message_id,status FROM model_usage
        WHERE session_id=?1 AND (?2 IS NULL OR turn_id=?2) AND query_source='main_turn'
        ORDER BY completed_at, id",
    )?;
    let mut last = None;
    for row in query.query_map(params![session, turn], |row| {
        Ok((
            Usage {
                provider: row.get(0)?,
                model: row.get(1)?,
                variant: row.get(2)?,
                turn: row.get(3)?,
                message: row.get(4)?,
            },
            row.get::<_, String>(5)?,
        ))
    })? {
        let (usage, status) = row?;
        verify_model(
            model,
            selected,
            &usage.provider,
            &usage.model,
            usage.variant.as_deref(),
            true,
        )?;
        if status == "completed" {
            last = Some(usage);
        }
    }
    Ok(last)
}

pub(crate) fn save_receipt(dir: &Path, evidence: &Value) -> Result<()> {
    save_json(dir, "zcode-model.json", evidence).context("无法写入 ZCode 实际模型回执")?;
    save_json(dir, "session.json", evidence).context("无法写入 ZCode 实际模型回执")
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
    let selected = zcode_receipt::selected(Some(model));
    let usage = main_turn_usage(db, task, Some(&turn), Some(model), &selected)?
        .context("缺少 ZCode 主对话模型用量回执")?;
    let id = usage.message.context("缺少 ZCode 助手消息 ID")?;
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
        "providerId":usage.provider,"modelId":usage.model,"reasoningLevel":usage.variant,
        "workspace":workspace,"selected":selected,"source":"model_usage"}),
        text,
    }))
}

/// Collect an already completed, pinned desktop input without creating a session or sending a prompt.
pub fn completed(dir: &Path, workspace: &Path, selected: &str) -> Result<String> {
    let home = std::env::home_dir()
        .context("missing home directory")?
        .join(".zcode");
    let index = readonly(&home.join("v2/tasks-index.sqlite"))?;
    let db = readonly(&home.join("cli/db/db.sqlite"))?;
    complete_with_db(dir, workspace, selected, &index, &db)
}

fn complete_with_db(
    dir: &Path,
    workspace: &Path,
    selected: &str,
    index: &Connection,
    db: &Connection,
) -> Result<String> {
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
            && matches!(job["mode"].as_str(), Some("edit" | "yolo"))
            && format!(
                "{}/{}",
                model["providerId"].as_str().unwrap_or(""),
                model["modelId"].as_str().unwrap_or("")
            ) == expected,
        "saved desktop task does not match the original workspace and model"
    );
    let dispatched = find_message(
        index,
        db,
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
    let reply = read_reply(db, workspace, session, input, model)?
        .context("original desktop turn has not completed")?;
    save_receipt(dir, &reply.evidence)?;
    Ok(reply.text)
}

pub fn converse(
    dir: &Path,
    workspace: &Path,
    mode: &str,
    model: &Value,
    prompt: &str,
    repository: Option<&Path>,
    resume: Option<&Value>,
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
        "edit" | "yolo" => "实现",
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
        let mut job = json!({"requestId":marker,"workspace":workspace,"dispatch":dispatch,"model":model,"mode":mode,
            "since":chrono::Utc::now().timestamp_millis()});
        if let Some(session) = resume {
            job["session"] = session.clone();
        }
        save_json(dir, "desktop-request.json", &job)?;
        job
    };
    let (task, message) = crate::zcode_desktop_send::dispatch(
        &index,
        &db,
        dir,
        &job,
        job["since"].as_i64().unwrap_or(0),
        repository,
    )?;
    save_json(
        dir,
        "desktop-task.json",
        &json!({"session":task,"userMessage":message,"workspace":workspace}),
    )?;
    // A continued task is already grouped under the item's ZCode project.
    if let Some(repo) = repository.filter(|_| resume.is_none())
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
            save_receipt(dir, &reply.evidence)?;
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
        let expected = reply
            .evidence
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>();
        let temp = tempfile::tempdir()?;
        let dir = temp.path();
        save_json(
            dir,
            "desktop-task.json",
            &json!({"session":"s","userMessage":"user","workspace":"/checkout"}),
        )?;
        save_json(
            dir,
            "desktop-create.json",
            &json!({"requestId":"job","session":{"workspacePath":"/checkout","sessionId":"s"}}),
        )?;
        for provider in [
            "account:bigmodel-start-plan",
            "account:bigmodel-individual-coding-plan",
        ] {
            db.execute(
                "UPDATE model_usage SET provider_id=?1 WHERE id=2",
                [provider],
            )?;
            let model = json!({"providerId":provider,"modelId":"glm"});
            let job = json!({"requestId":"job","workspace":"/checkout","mode":"edit","model":model,
                "since":1,"dispatch":"unique dispatch"});
            save_json(dir, "desktop-request.json", &job)?;
            let selected = format!("{provider}/glm");
            assert_eq!(
                complete_with_db(dir, Path::new("/checkout"), &selected, &db, &db)?,
                "handoff"
            );
            let receipt = aep::workspace::read(&dir.join("session.json"))?;
            assert_eq!(
                receipt.as_object().unwrap().keys().collect::<Vec<_>>(),
                expected
            );
            assert_eq!(receipt["selected"], selected);
            assert_eq!(receipt["source"], "model_usage");
            assert_eq!(
                receipt,
                aep::workspace::read(&dir.join("zcode-model.json"))?
            );
        }
        db.execute(
            "UPDATE model_usage SET provider_id='another plan' WHERE id=2",
            [],
        )?;
        let error = complete_with_db(
            dir,
            Path::new("/checkout"),
            "account:bigmodel-individual-coding-plan/glm",
            &db,
            &db,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("所选 account:bigmodel-individual-coding-plan/glm"));
        assert!(error.contains("实际 another plan / glm / max"));
        db.execute("UPDATE model_usage SET provider_id='plan' WHERE id=2", [])?;
        db.execute("UPDATE model_usage SET variant='low' WHERE id=2", [])?;
        assert!(read_reply(&db, "/checkout", "s", "user", &model).is_err());
        db.execute("DELETE FROM model_usage WHERE id=2", [])?;
        assert!(
            read_reply(&db, "/checkout", "s", "user", &model)
                .err()
                .unwrap()
                .to_string()
                .contains("缺少 ZCode 主对话模型用量回执")
        );
        Ok(())
    }

    #[test]
    fn app_server_usage_uses_last_completed_row_and_rejects_mismatched_retries() -> Result<()> {
        let db = Connection::open_in_memory()?;
        db.execute_batch(r#"
            CREATE TABLE model_usage(id,session_id,turn_id,query_source,status,completed_at,provider_id,model_id,variant,assistant_message_id);
            INSERT INTO model_usage VALUES
                (1,'app','turn','title','completed',4,'ignored','glm',NULL,'title'),
                (2,'app','turn','main_turn','completed',2,'custom','glm','low','first'),
                (3,'app','turn','main_turn','completed',3,'custom','glm','max','last');
        "#)?;
        assert!(main_turn_usage(&db, "missing", None, None, "")?.is_none());
        let usage = main_turn_usage(&db, "app", None, None, "")?.unwrap();
        assert_eq!(usage.turn, "turn");
        assert_eq!(usage.message.as_deref(), Some("last"));
        assert_eq!(usage.variant.as_deref(), Some("max"));
        let model =
            json!({"providerId":"custom","modelId":"glm","options":{"reasoningLevel":"max"}});
        assert!(main_turn_usage(&db, "app", None, Some(&model), "custom/glm@max").is_err());
        db.execute("UPDATE model_usage SET status='error' WHERE id=2", [])?;
        assert!(main_turn_usage(&db, "app", None, Some(&model), "custom/glm@max").is_err());
        Ok(())
    }
}
