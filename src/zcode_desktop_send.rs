//! Dispatch through the installed ZCode extension, then reconcile its receipt with desktop history.
use anyhow::{Context, Result, bail, ensure};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    fs::{self, DirBuilder},
    io::{BufRead, BufReader, Write},
    os::unix::{
        fs::{DirBuilderExt, MetadataExt},
        net::UnixListener,
    },
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

pub(super) fn project_id(index: &Connection, workspace: &str) -> Result<Option<String>> {
    let mut q = index.prepare(
        "SELECT p.project_id FROM projects p JOIN hosts h USING(host_id)
        JOIN project_roots r USING(project_id) WHERE p.deleted=0 AND h.host_id='local'
        AND h.kind='local' AND r.is_primary=1 AND r.root_path=?1 LIMIT 2",
    )?;
    let mut rows = q.query([workspace])?;
    let first = rows.next()?.map(|row| row.get(0)).transpose()?;
    ensure!(
        rows.next()?.is_none(),
        "多个 ZCode 项目以 {workspace} 为主目录"
    );
    Ok(first)
}

pub(super) fn local_project(index: &Connection, workspace: &str) -> Result<String> {
    if let Some(id) = project_id(index, workspace)? {
        return Ok(id);
    }
    let status = Command::new("/usr/bin/open")
        .args([
            "-g",
            "-n",
            "-a",
            "/Applications/ZCode.app",
            "--args",
            "--open-workspace",
            workspace,
        ])
        .status()?;
    ensure!(status.success(), "无法打开 ZCode 工作区 {workspace}");
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(20) {
        if let Some(id) = project_id(index, workspace)? {
            return Ok(id);
        }
        thread::sleep(Duration::from_millis(250));
    }
    bail!("ZCode 未登记工作区 {workspace}")
}

struct SocketPath(PathBuf);
impl Drop for SocketPath {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub(super) fn exchange(root: &Path, request: &Value) -> Result<Value> {
    DirBuilder::new().recursive(true).mode(0o700).create(root)?;
    let meta = fs::symlink_metadata(root)?;
    ensure!(
        meta.is_dir() && meta.uid() == nix::unistd::getuid().as_raw() && meta.mode() & 0o077 == 0,
        "ZCode 派发目录必须是仅当前用户可访问的目录：{}",
        root.display()
    );
    let id = request["requestId"].as_str().context("缺少派发 ID")?;
    ensure!(
        id.len() == 32 && id.bytes().all(|c| c.is_ascii_hexdigit()),
        "无效的派发 ID"
    );
    // The caller holds the attempt's exclusive lock, including during crash recovery.
    let path = root.join(format!("{id}.sock"));
    if path.try_exists()? {
        fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path)?;
    let _socket = SocketPath(path);
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut socket = loop {
        match listener.accept() {
            Ok((socket, _)) => break socket,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(100))
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => bail!(
                "未连接 ZCode 的 aep-studio 扩展。请打开 ZCode，安装并启用扩展，再在插件详情页授权。"
            ),
            Err(e) => return Err(e.into()),
        }
    };
    drop(_socket);
    // Darwin propagates the listening socket's O_NONBLOCK flag to accepted streams.
    socket.set_nonblocking(false)?;
    socket.set_write_timeout(Some(Duration::from_secs(10)))?;
    socket.set_read_timeout(Some(Duration::from_secs(90)))?;
    writeln!(socket, "{request}")?;
    let mut line = String::new();
    BufReader::new(socket)
        .read_line(&mut line)
        .context("等待 ZCode 扩展回执")?;
    let reply: Value = serde_json::from_str(&line).context("ZCode 扩展未返回有效回执")?;
    if let Some(error) = reply["error"].as_str() {
        let hint = match reply["code"].as_str() {
            Some("not-granted") => "。请在 ZCode 插件详情页授权",
            _ if request["action"].is_string() && error == "Invalid AEP dispatch" => {
                "。请更新并重新载入 aep-studio 扩展"
            }
            _ => "",
        };
        let message = format!("ZCode 扩展请求失败（{}）：{error}{hint}", reply["code"]);
        if matches!(
            reply["code"].as_str(),
            Some("invalid-input" | "not-granted" | "forbidden")
        ) {
            return Err(anyhow::Error::new(DispatchRejected).context(message));
        }
        bail!(message);
    }
    Ok(reply)
}

#[derive(Debug)]
struct DispatchRejected;
impl std::fmt::Display for DispatchRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("派发已被 ZCode 拒绝")
    }
}
impl std::error::Error for DispatchRejected {}

fn create_session(root: &Path, request: &Value) -> Result<Value> {
    let reply = exchange(root, request)?;
    let id = &request["requestId"];
    if reply["creation"] == "rejected" || reply["firstInput"] == "rejected" {
        return Err(anyhow::Error::new(DispatchRejected).context(format!(
            "ZCode 拒绝创建任务或发送首条消息：{}",
            reply["reason"]
        )));
    }
    let session = &reply["session"];
    ensure!(
        &reply["requestId"] == id
            && session["sourceServerId"] == "local"
            && session["project"]["projectId"] == request["projectId"]
            && session["sessionId"]
                .as_str()
                .is_some_and(|id| !id.is_empty()),
        "ZCode 返回的项目或派发回执不匹配"
    );
    Ok(reply)
}

/// Marker for a continuation the installed extension or ZCode could not accept; the runner then
/// starts a new task with the full context.
pub const CONTINUE_FAILED: &str = "zcode desktop continue failed";

/// Send the next prompt to an existing desktop task (extension 1.3: `sendToSession`).
fn send_to_session(root: &Path, request: &Value) -> Result<Value> {
    let reply = exchange(root, request).map_err(|e| anyhow::anyhow!("{CONTINUE_FAILED}: {e:#}"))?;
    let session = &reply["session"];
    ensure!(
        reply["admission"] != "rejected",
        "{CONTINUE_FAILED}: ZCode 没有接受继续本任务：{}",
        reply["reason"]
    );
    ensure!(
        reply["requestId"] == request["requestId"]
            && session["sessionId"] == request["session"]["sessionId"]
            && reply["operation"]["session"]["sessionId"] == request["session"]["sessionId"],
        "{CONTINUE_FAILED}: ZCode 返回的继续回执不匹配"
    );
    Ok(reply)
}

fn session_action(receipt: &Value, action: &str) -> Result<Value> {
    let session = &receipt["session"];
    ensure!(
        session["sessionId"].is_string() && receipt["operation"]["session"] == *session,
        "创建回执缺少对应的 ZCode 会话或操作"
    );
    desktop_action(session, &receipt["operation"], action)
}

fn desktop_action(session: &Value, operation: &Value, action: &str) -> Result<Value> {
    let root = std::env::home_dir()
        .context("缺少用户目录")?
        .join(".aep/zcode");
    let id = uuid::Uuid::new_v4().simple().to_string();
    // The creation receipt is immutable. Resolve current project membership in the host
    // when opening or observing a task that has since moved.
    let mut session = session.clone();
    session
        .as_object_mut()
        .context("invalid desktop session")?
        .remove("project");
    let mut operation = operation.clone();
    operation["session"] = session.clone();
    let reply = exchange(
        &root,
        &json!({"action":action,"requestId":id,"session":session,
        "operation":operation}),
    )?;
    ensure!(
        reply["requestId"] == id && reply["sessionId"] == session["sessionId"],
        "ZCode 操作回执不匹配"
    );
    if action == "approvals" {
        ensure!(
            reply["waiting"].is_boolean() && reply["tools"].is_array(),
            "ZCode 扩展返回的审批状态无效"
        );
    }
    Ok(reply)
}

/// Archive the ZCode desktop tasks saved in these role or stage evidence directories, each
/// task once. Used when an AEP item is archived; one task failing does not stop the rest.
pub fn archive_tasks(dirs: &[std::path::PathBuf]) -> Vec<Value> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(receipt) = aep::workspace::read(&dir.join("desktop-create.json")) else {
            continue;
        };
        let session = &receipt["session"];
        let Some(id) = session["sessionId"].as_str() else {
            continue;
        };
        if session["sourceServerId"] != "local" || !seen.insert(id.to_owned()) {
            continue;
        }
        let row = match desktop_action(session, &json!({"sessionId":id}), "archive") {
            Ok(reply) => json!({"session":id,"status":reply["status"]}),
            Err(e) => json!({"session":id,"error":format!("{e:#}")}),
        };
        out.push(row);
    }
    out
}

pub fn open_task(dir: &Path) -> Result<Value> {
    // A saved session can be opened after both Studio and the extension host quit.
    let app = Path::new("/Applications/ZCode.app");
    if cfg!(target_os = "macos") && app.is_dir() {
        let status = Command::new("/usr/bin/open")
            .args(["-g", "-a"])
            .arg(app)
            .status()?;
        ensure!(status.success(), "无法启动 ZCode 桌面扩展");
    }
    // Desktop sessions predating automatic dispatch have a completed session
    // receipt but no creation command. Opening only needs their native identity.
    let open = || -> Result<Value> {
        let path = dir.join("desktop-create.json");
        if path.try_exists()? {
            return session_action(&aep::workspace::read(&path)?, "open");
        }
        let saved = aep::workspace::read(&dir.join("session.json"))?;
        ensure!(saved["transport"] == "desktop", "此记录不是 ZCode 桌面会话");
        let id = saved["session"].as_str().context("缺少会话 ID")?;
        let workspace = saved["workspace"].as_str().context("缺少工作区")?;
        let session =
            json!({"kind":"app","sourceServerId":"local","sessionId":id,"workspacePath":workspace});
        desktop_action(&session, &json!({"sessionId":id}), "open")
    };
    let mut reply = open()?;
    if reply["reason"] == "project.task-not-found" && dir.join("desktop-cleanup.json").is_file() {
        // Focusing the desktop refreshes its sidebar projection after a background move.
        let status = Command::new("/usr/bin/open")
            .args(["-a", "/Applications/ZCode.app"])
            .status()?;
        if status.success() {
            for _ in 0..3 {
                thread::sleep(Duration::from_millis(350));
                reply = open()?;
                if reply["reason"] != "project.task-not-found" {
                    break;
                }
            }
        }
    }
    ensure!(
        reply["status"] == "opened",
        "ZCode 无法打开该任务：{}",
        reply["reason"]
    );
    Ok(reply)
}

/// Observe approval state independently of reply collection. An unavailable
/// extension must not discard a completed model reply or resend the prompt.
pub fn monitor(dir: &Path, task: &str) -> Option<std::sync::mpsc::Sender<()>> {
    let receipt = aep::workspace::read(&dir.join("desktop-create.json")).ok()?;
    if receipt["session"]["sessionId"] != task {
        return None;
    }
    let dir = dir.to_owned();
    let (stop, stopped) = std::sync::mpsc::channel();
    thread::spawn(move || {
        loop {
            let state = match session_action(&receipt, "approvals") {
                Ok(reply) => json!({"waiting":reply["waiting"],"tools":reply["tools"]}),
                Err(e) => json!({"error":format!("{e:#}")}),
            };
            if stopped.try_recv() != Err(std::sync::mpsc::TryRecvError::Empty) {
                break;
            }
            let state = json!({"session":receipt["session"]["sessionId"],"workspace":receipt["session"]["workspacePath"],
                "observedAt":chrono::Utc::now().timestamp(), "state":state});
            let _ = aep::project::save_json(&dir, "desktop-approvals.json", &state);
            if stopped.recv_timeout(Duration::from_secs(2))
                != Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            {
                break;
            }
        }
    });
    Some(stop)
}

#[cfg_attr(not(test), allow(dead_code))]
/// The desktop task mode for a stage mode. The desktop's own modes are build ("Ask before
/// changes", its default), guarded ("Autonomous mode", asks only when risky) and yolo ("Full
/// access"). Like Claude and Codex in the default sandbox, writing stages and gates work without
/// asking for each edit; a read-only stage keeps the default and is told not to edit.
fn desktop_mode(mode: &str) -> Option<&'static str> {
    match mode {
        "yolo" => Some("yolo"),
        "edit" | "build" | "guarded" => Some("guarded"),
        _ => None,
    }
}

pub fn dispatch(
    index: &Connection,
    db: &Connection,
    dir: &Path,
    job: &Value,
    since: i64,
    repository: Option<&Path>,
) -> Result<(String, String)> {
    let workspace = job["workspace"].as_str().context("缺少工作区")?;
    let prompt = job["dispatch"].as_str().context("缺少派发文本")?;
    let locate = || super::zcode_desktop::find_message(index, db, workspace, since, prompt);
    let receipt = dir.join("desktop-create.json");
    if !receipt.try_exists()?
        && let Some(pair) = locate()?
    {
        return Ok(pair);
    }
    ensure!(
        job["requestId"].is_string(),
        "旧版手动派发尚无桌面回执，请在 Studio 重试本轮"
    );
    let reply = if receipt.try_exists()? {
        aep::workspace::read(&receipt)?
    } else if job["session"].is_object() {
        // The next round of the same stage continues its task (history, model, mode kept).
        let root = std::env::home_dir()
            .context("缺少用户目录")?
            .join(".aep/zcode");
        let reply = send_to_session(
            &root,
            &json!({"action":"send","requestId":job["requestId"],"session":job["session"],"prompt":prompt}),
        )?;
        aep::project::save_json(dir, "desktop-create.json", &reply)?;
        reply
    } else {
        let source = repository
            .map(|repo| crate::zcode_group::staging::prepare(dir, job, repo))
            .transpose()?
            .flatten();
        let project = match source {
            Some(id) => {
                // Wake the extension host without opening a folder or reserving a draft.
                let status = Command::new("/usr/bin/open")
                    .args(["-g", "-a", "/Applications/ZCode.app"])
                    .status()?;
                ensure!(status.success(), "无法启动 ZCode 桌面扩展");
                id
            }
            None => local_project(index, workspace)?,
        };
        let model = &job["model"];
        let root = std::env::home_dir()
            .context("缺少用户目录")?
            .join(".aep/zcode");
        // ZCode lets a plugin choose the permission mode only for sessions in the plugin's own
        // project ("forbidden" otherwise), so an item task takes its ZCode project's mode, which
        // the person sets once in ZCode. `desktop_mode` stays for the plugin's own project.
        let request = json!({"requestId":job["requestId"],"projectId":project,
            "providerId":model["providerId"],"modelId":model["modelId"],"prompt":prompt});
        let reply = create_session(&root, &request).inspect_err(|err| {
            if err.is::<DispatchRejected>()
                && let Some(repo) = repository
                && dir.join("desktop-source-project.json").is_file()
            {
                let saved = aep::project::save_json(
                    dir,
                    "desktop-declined.json",
                    &json!({"requestId":job["requestId"],"project":project}),
                );
                if let Err(e) = saved.and_then(|()| crate::zcode_group::staging::cleanup(dir, repo))
                {
                    eprintln!("ZCode 空项目清理失败：{e:#}");
                }
            }
        })?;
        aep::project::save_json(dir, "desktop-create.json", &reply)?;
        reply
    };
    let start = Instant::now();
    loop {
        if let Some(pair) = locate()? {
            ensure!(
                reply["session"]["sessionId"] == pair.0,
                "桌面任务与扩展回执不符"
            );
            return Ok(pair);
        }
        ensure!(
            start.elapsed() < Duration::from_secs(40),
            "桌面已受理任务 {}，尚未找到对应派发消息",
            reply["session"]["sessionId"]
        );
        thread::sleep(Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    #[test]
    fn waits_for_delayed_extension_ack_after_accept() -> Result<()> {
        let temp = tempfile::tempdir_in("/tmp")?;
        let folder = temp.path().join("queue");
        let id = uuid::Uuid::new_v4().simple().to_string();
        let path = folder.join(format!("{id}.sock"));
        let job = json!({"requestId":id,"projectId":"project"});
        let client = thread::spawn(move || -> Result<()> {
            let deadline = Instant::now() + Duration::from_secs(3);
            // The socket file appears at bind, just before listen: retry a refused connect.
            let mut socket = loop {
                match UnixStream::connect(&path) {
                    Ok(socket) => break socket,
                    Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
                    Err(error) => return Err(error.into()),
                }
            };
            let mut line = String::new();
            BufReader::new(&socket).read_line(&mut line)?;
            let input: Value = serde_json::from_str(&line)?;
            thread::sleep(Duration::from_millis(50));
            writeln!(
                socket,
                "{}",
                json!({"creation":"created","firstInput":"accepted",
                "requestId":input["requestId"],"session":{"sessionId":"sess-1",
                "sourceServerId":"local","project":{"projectId":input["projectId"]}}})
            )?;
            Ok(())
        });
        let result = create_session(&folder, &job);
        client.join().unwrap()?;
        assert_eq!(result?["session"]["sessionId"], "sess-1");
        assert_eq!(fs::read_dir(folder)?.count(), 0);
        Ok(())
    }

    /// A stand-in extension: reads one request, answers with `reply(request)`.
    fn extension(path: PathBuf, reply: fn(&Value) -> Value) -> thread::JoinHandle<Result<Value>> {
        thread::spawn(move || -> Result<Value> {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut socket = loop {
                match UnixStream::connect(&path) {
                    Ok(socket) => break socket,
                    Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
                    Err(error) => return Err(error.into()),
                }
            };
            let mut line = String::new();
            BufReader::new(&socket).read_line(&mut line)?;
            let input: Value = serde_json::from_str(&line)?;
            writeln!(socket, "{}", reply(&input))?;
            Ok(input)
        })
    }

    #[test]
    fn continuing_a_task_sends_to_its_session_and_checks_the_receipt() -> Result<()> {
        let temp = tempfile::tempdir_in("/tmp")?;
        let folder = temp.path().join("queue");
        let session = json!({"kind":"app","sourceServerId":"local","sessionId":"sess_1"});
        let request =
            |id: &str| json!({"action":"send","requestId":id,"session":session,"prompt":"T2"});

        let id = uuid::Uuid::new_v4().simple().to_string();
        let ext = extension(folder.join(format!("{id}.sock")), |input| {
            json!({"requestId":input["requestId"],"session":input["session"],"admission":"accepted",
                   "operation":{"commandId":"c2","session":input["session"]}})
        });
        let reply = send_to_session(&folder, &request(&id))?;
        let sent = ext.join().unwrap()?;
        assert_eq!(sent["action"], "send");
        assert_eq!(sent["session"]["sessionId"], "sess_1");
        assert_eq!(reply["operation"]["commandId"], "c2");

        // Rejected, or an older extension that does not know "send": the runner falls back.
        let id = uuid::Uuid::new_v4().simple().to_string();
        let ext = extension(
            folder.join(format!("{id}.sock")),
            |input| json!({"requestId":input["requestId"],"session":input["session"],"admission":"rejected","reason":"busy"}),
        );
        let err = send_to_session(&folder, &request(&id)).unwrap_err();
        ext.join().unwrap()?;
        assert!(err.to_string().contains(CONTINUE_FAILED), "{err}");
        let id = uuid::Uuid::new_v4().simple().to_string();
        let ext = extension(
            folder.join(format!("{id}.sock")),
            |_| json!({"error":"Invalid AEP dispatch","code":"extension-error"}),
        );
        let err = send_to_session(&folder, &request(&id)).unwrap_err();
        ext.join().unwrap()?;
        assert!(format!("{err:#}").contains(CONTINUE_FAILED), "{err:#}");
        Ok(())
    }

    #[test]
    fn desktop_tasks_get_the_stage_mode() {
        assert_eq!(desktop_mode("yolo"), Some("yolo"));
        assert_eq!(desktop_mode("edit"), Some("guarded"));
        assert_eq!(desktop_mode("guarded"), Some("guarded"));
        assert_eq!(desktop_mode("plan"), None);
    }
}
