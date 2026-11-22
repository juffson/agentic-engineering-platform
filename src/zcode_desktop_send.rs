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

fn project_id(index: &Connection, workspace: &str) -> Result<Option<String>> {
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

fn local_project(index: &Connection, workspace: &str) -> Result<String> {
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

fn create_session(root: &Path, request: &Value) -> Result<Value> {
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
        .context("等待 ZCode 扩展创建回执")?;
    let reply: Value = serde_json::from_str(&line).context("ZCode 扩展未返回创建回执")?;
    if let Some(error) = reply["error"].as_str() {
        bail!(
            "ZCode 扩展派发失败（{}）：{error}。请检查插件详情页的会话权限。",
            reply["code"]
        );
    }
    ensure!(
        reply["creation"] != "rejected" && reply["firstInput"] != "rejected",
        "ZCode 拒绝创建任务或发送首条消息：{}",
        reply["reason"]
    );
    let session = &reply["session"];
    ensure!(
        reply["requestId"] == id
            && session["sourceServerId"] == "local"
            && session["project"]["projectId"] == request["projectId"]
            && session["sessionId"]
                .as_str()
                .is_some_and(|id| !id.is_empty()),
        "ZCode 返回的项目或派发回执不匹配"
    );
    Ok(reply)
}

pub fn dispatch(
    index: &Connection,
    db: &Connection,
    dir: &Path,
    job: &Value,
    since: i64,
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
    } else {
        let project = local_project(index, workspace)?;
        let model = &job["model"];
        let root = std::env::home_dir()
            .context("缺少用户目录")?
            .join(".aep/zcode");
        let reply = create_session(
            &root,
            &json!({"requestId":job["requestId"],"projectId":project,
            "providerId":model["providerId"],"modelId":model["modelId"],"prompt":prompt}),
        )?;
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
            while !path.exists() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(5));
            }
            let mut socket = UnixStream::connect(path)?;
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
}
