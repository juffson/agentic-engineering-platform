//! Read ZCode's resolved model list, without sending a prompt or reading its credentials.
use crate::workflow::{Client, Profile, ZCODE_APP};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    ffi::OsStr,
    io::{BufRead, BufReader, Write},
    os::unix::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRef {
    pub provider_id: String,
    pub model_id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Level {
    pub value: String,
    pub label: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reasoning {
    #[serde(default)]
    pub levels: Vec<Level>,
    pub default_level: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    #[serde(rename = "ref")]
    pub id: ModelRef,
    pub label: String,
    pub provider_label: Option<String>,
    #[serde(default)]
    pub reasoning: Reasoning,
}

impl Model {
    pub fn selection(&self, level: Option<&str>) -> String {
        let base = format!("{}/{}", self.id.provider_id, self.id.model_id);
        level.map_or_else(|| base.clone(), |l| format!("{base}@{l}"))
    }
    pub fn default_selection(&self) -> String {
        let levels = &self.reasoning.levels;
        self.selection(
            self.reasoning
                .default_level
                .as_deref()
                .filter(|l| levels.iter().any(|v| v.value == *l))
                .or_else(|| levels.first().map(|l| l.value.as_str())),
        )
    }
    pub fn accepts(&self, selection: &str) -> bool {
        (self.reasoning.levels.is_empty() && selection == self.selection(None))
            || self
                .reasoning
                .levels
                .iter()
                .any(|l| selection == self.selection(Some(&l.value)))
    }
}

pub fn app_server(binary: &Path) -> Command {
    let mut command = Command::new(binary);
    if (binary == Path::new(ZCODE_APP)
        || binary
            .canonicalize()
            .is_ok_and(|p| p == Path::new(ZCODE_APP)))
        && std::env::var_os("ZCODE_BUILTIN_PROVIDER_CONFIG_FILE").is_none()
    {
        command.env(
            "ZCODE_BUILTIN_PROVIDER_CONFIG_FILE",
            "/Applications/ZCode.app/Contents/Resources/config/provider/zcode-builtin.json",
        );
    }
    command.arg("app-server");
    command
}

pub fn client_response(method: &str) -> Result<Value> {
    match method {
        "session/requestRuntimePreferences" => Ok(json!({"nativeSearchEnhancementsEnabled":false})),
        "interaction/requestOfficialMcpAuthHeaders" => Ok(json!({})),
        "session/readReplyLanguage" => Ok(json!({"language":null})),
        _ => bail!("ZCode 需要客户端交互 {method}，请核对本机工具权限后重试"),
    }
}

pub fn discover(path: &OsStr) -> Result<Vec<Model>> {
    let profile = Profile {
        client: Client::ZCode,
        executable: String::new(),
        model: String::new(),
        check_command: String::new(),
    };
    let binary = profile.program(path)?;
    let workspace = tempfile::tempdir()?;
    let mut child = app_server(&binary)
        .current_dir(workspace.path())
        .env("PATH", path)
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("启动 ZCode 模型目录")?;
    let mut input = child.stdin.take().unwrap();
    let output = child.stdout.take().unwrap();
    let cwd = workspace.path().to_path_buf();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| -> Result<Vec<Model>> {
            let mut send = |msg: Value| -> Result<()> {
                writeln!(input, "{msg}")?;
                input.flush()?;
                Ok(())
            };
            let session = format!("sess_{}", uuid::Uuid::new_v4().simple());
            send(
                json!({"id":1,"method":"session/create","params":{"sessionId":session,"persistence":"deferred","mcpServers":[],"toolAllowlist":[],"workspace":{"workspaceKey":cwd,"workspacePath":cwd},"titleGenerationEnabled":false,"mode":"plan"}}),
            )?;
            for line in BufReader::new(output).lines() {
                let msg: Value = serde_json::from_str(&line?)?;
                if let Some(method) = msg["method"].as_str() {
                    if !msg["id"].is_null() {
                        let reply = client_response(method)?;
                        send(json!({"id":msg["id"],"result":reply}))?;
                    }
                } else if msg["id"] == 1 {
                    if let Some(error) = msg.pointer("/error/message").and_then(Value::as_str) {
                        bail!("ZCode 模型目录：{error}");
                    }
                    let rows = msg
                        .pointer("/result/settings/model/available")
                        .and_then(Value::as_array)
                        .context("ZCode 未返回可用模型目录")?;
                    let models: Vec<Model> = rows
                        .iter()
                        .filter(|r| r["disabledReason"].is_null())
                        .map(|r| serde_json::from_value(r.clone()))
                        .collect::<Result<_, _>>()?;
                    ensure!(!models.is_empty(), "ZCode 未配置可用模型");
                    let _ = send(
                        json!({"id":2,"method":"session/close","params":{"sessionId":session,"expectedPersistence":"deferred"}}),
                    );
                    return Ok(models);
                }
            }
            bail!("ZCode 在返回模型目录前已退出")
        })();
        let _ = tx.send(result);
    });
    let result = rx
        .recv_timeout(Duration::from_secs(15))
        .context("等待 ZCode 模型目录失败");
    let _ = nix::sys::signal::killpg(
        nix::unistd::Pid::from_raw(child.id() as i32),
        nix::sys::signal::Signal::SIGKILL,
    );
    let _ = child.wait();
    result?
}
