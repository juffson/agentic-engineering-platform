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
        if desktop_plan(&self.id.provider_id).is_some() {
            return self.selection(None);
        }
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
        if desktop_plan(&self.id.provider_id).is_some() {
            return selection == self.selection(None);
        }
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
        command.env("ZCODE_BUILTIN_PROVIDER_CONFIG_FILE", BUILTIN);
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

fn resolved_models(path: &OsStr) -> Result<Vec<Model>> {
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

const BUILTIN: &str =
    "/Applications/ZCode.app/Contents/Resources/config/provider/zcode-builtin.json";

pub fn desktop_plan(id: &str) -> Option<&'static str> {
    match id {
        "account:bigmodel-start-plan" => Some("BigModel Start Plan"),
        "account:bigmodel-individual-coding-plan" => Some("BigModel 个人 Coding Plan"),
        _ => None,
    }
}

fn rows<'a>(doc: &'a Value, path: &str) -> impl Iterator<Item = &'a Value> {
    doc.pointer(path)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn desktop_models(local: &Value, builtin: &Value) -> Vec<Model> {
    let mut result = Vec::new();
    let rules: Vec<_> = rows(builtin, "/config/modelConfigRules/modelRules")
        .filter_map(|r| {
            Some((
                regex::RegexBuilder::new(r["modelMatch"].as_str()?)
                    .case_insensitive(true)
                    .build()
                    .ok()?,
                r.pointer("/config/optionSpecs/reasoningLevel/values")?
                    .as_array()?,
            ))
        })
        .collect();
    for provider in rows(local, "/config/providerConfigRules/providerRules") {
        if provider["config"]["enabled"] == false {
            continue;
        }
        let Some(id) = provider["providerId"].as_str() else {
            continue;
        };
        let Some(label) = desktop_plan(id) else {
            continue;
        };
        let Some(base) = rows(builtin, "/config/providerConfigRules/providerRules")
            .find(|p| p["providerId"] == id)
        else {
            continue;
        };
        let builtin_ids = rows(base, "/config/builtinModelIds").collect::<Vec<_>>();
        let personal = rows(provider, "/config/personalModelIds").collect::<Vec<_>>();
        let mut order = rows(provider, "/config/modelOrder").collect::<Vec<_>>();
        if order.is_empty() {
            order.extend(builtin_ids.iter().copied());
        }
        order.extend(personal.iter().copied());
        let mut seen = std::collections::HashSet::new();
        for model in order {
            let Some(name) = model.as_str() else { continue };
            if !seen.insert(name) || !(builtin_ids.contains(&model) || personal.contains(&model)) {
                continue;
            }
            if rows(local, "/config/modelConfigRules/providerModelRules").any(|r| {
                r["modelId"] == name && r["providerId"] == id && r["config"]["enabled"] == false
            }) {
                continue;
            }
            let levels = rules
                .iter()
                .rev()
                .find(|(re, _)| re.is_match(name))
                .into_iter()
                .flat_map(|(_, levels)| levels.iter())
                .filter_map(Value::as_str)
                .map(|v| Level {
                    value: v.to_owned(),
                    label: v.to_owned(),
                })
                .collect();
            result.push(Model {
                id: ModelRef {
                    provider_id: id.to_owned(),
                    model_id: name.to_owned(),
                },
                label: name.to_owned(),
                provider_label: Some(label.to_owned()),
                reasoning: Reasoning {
                    levels,
                    default_level: None,
                },
            });
        }
    }
    result
}

pub fn discover(path: &OsStr) -> Result<Vec<Model>> {
    let cli = resolved_models(path);
    let Some(config) = std::env::home_dir()
        .map(|h| h.join(".zcode/v2/provider_config.json"))
        .filter(|p| p.is_file() && Path::new(BUILTIN).is_file())
    else {
        let models = cli?;
        ensure!(!models.is_empty(), "ZCode 未配置可用模型");
        return Ok(models);
    };
    let desktop = desktop_models(
        &crate::workspace::read(&config)?,
        &crate::workspace::read(Path::new(BUILTIN))?,
    );
    let mut models = if desktop.is_empty() {
        cli?
    } else {
        cli.unwrap_or_default()
    };
    for model in desktop {
        if let Some(entry) = models.iter_mut().find(|m| {
            m.id.provider_id == model.id.provider_id && m.id.model_id == model.id.model_id
        }) {
            entry.provider_label = model.provider_label;
        } else {
            models.push(model);
        }
    }
    ensure!(!models.is_empty(), "ZCode 未配置可用模型");
    Ok(models)
}

/// Old project defaults can include a level from the manual desktop composer.
pub fn effective_selection(selection: &str) -> &str {
    if let Some((provider, _)) = selection.split_once('/')
        && desktop_plan(provider).is_some()
        && let Some((model, _)) = selection.rsplit_once('@')
    {
        return model;
    }
    selection
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_stale_order_entries_and_keeps_model_ids_scoped_to_each_plan() {
        let start = "account:bigmodel-start-plan";
        let individual = "account:bigmodel-individual-coding-plan";
        let local = json!({"config":{
          "providerConfigRules":{"providerRules":[
            {"providerId":start,"config":{"modelOrder":["stale","flash"]}},
            {"providerId":individual,"config":{"personalModelIds":["extra","off"],"modelOrder":["flash"]}}
          ]},
          "modelConfigRules":{"providerModelRules":[{"providerId":individual,"modelId":"off","config":{"enabled":false}}]}
        }});
        let builtin = json!({"config":{
          "providerConfigRules":{"providerRules":[
            {"providerId":start,"config":{"builtinModelIds":["flash"]}},
            {"providerId":individual,"config":{"builtinModelIds":["flash"]}}
          ]},
          "modelConfigRules":{"modelRules":[{"modelMatch":"^FLASH$","config":{"optionSpecs":{"reasoningLevel":{"values":["low","max"]}}}}]}
        }});
        let models = desktop_models(&local, &builtin);
        assert_eq!(models.len(), 3);
        assert_eq!(models[0].default_selection(), format!("{start}/flash"));
        assert!(!models[0].accepts(&format!("{start}/flash@max")));
        assert!(models[1].accepts(&format!("{individual}/flash")));
        assert!(models[2].accepts(&format!("{individual}/extra")));
    }
}
