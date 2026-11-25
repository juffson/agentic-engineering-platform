//! Use the installed ZCode app-server with its native settings or a pinned model.
use crate::zcode_desktop::{main_turn_usage, readonly, save_receipt, verify_model};
use aep::{
    workflow::{Request, Role},
    zcode_receipt,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};

/// Run a configured desktop plan in its native task, or an ordinary CLI model via app-server.
#[allow(clippy::too_many_arguments)]
pub fn converse(
    binary: &Path,
    dir: &Path,
    mode: &str,
    model: Option<&Value>,
    prompt: &str,
    desktop_workspace: &Path,
    repository: Option<&Path>,
    resume: Option<&str>,
) -> Result<String> {
    let result = if Path::new(aep::workflow::ZCODE_APP).is_file()
        && let Some(model) = model
        && model["providerId"]
            .as_str()
            .and_then(aep::zcode_catalog::desktop_plan)
            .is_some()
    {
        let selection = json!({"providerId":model["providerId"], "modelId":model["modelId"]});
        // Desktop plan tasks cannot be continued (R7): let the runner resend full context.
        if resume.is_some() {
            eprintln!("{RESUME_FAILED}: 桌面任务不支持继续原会话");
            bail!("{RESUME_FAILED}: 桌面任务不支持继续原会话");
        }
        crate::zcode_desktop::converse(dir, desktop_workspace, mode, &selection, prompt, repository)
    } else {
        app_server(binary, dir, mode, model, prompt, resume)
    };
    result.inspect_err(|error| {
        let _ = aep::project::save_json(
            dir,
            "desktop-error.json",
            &json!({"error":format!("{error:#}")}),
        );
    })
}

fn app_server(
    binary: &Path,
    dir: &Path,
    mode: &str,
    model: Option<&Value>,
    prompt: &str,
    resume: Option<&str>,
) -> Result<String> {
    let mut child = aep::zcode_catalog::app_server(binary)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("启动 ZCode app-server")?;
    let talk = talk(&mut child, dir, mode, model, prompt, resume);
    let _ = child.kill();
    let _ = child.wait();
    talk
}

/// Marker the stage runner looks for to fall back to a new session with full context.
pub const RESUME_FAILED: &str = "zcode session resume failed";

fn talk(
    child: &mut std::process::Child,
    dir: &Path,
    mode: &str,
    model: Option<&Value>,
    prompt: &str,
    resume: Option<&str>,
) -> Result<String> {
    // The outer supervisor owns this process group and its deadline/cancel marker.
    {
        let mut input = child.stdin.take().context("ZCode stdin")?;
        let output = BufReader::new(child.stdout.take().context("ZCode stdout")?);
        let cwd = std::env::current_dir()?;
        let session = resume.map_or_else(
            || format!("sess_{}", uuid::Uuid::new_v4().simple()),
            str::to_owned,
        );
        let selected = zcode_receipt::selected(model);
        let mut settings = Value::Null;
        let workspace = json!({"workspaceKey":cwd,"workspacePath":cwd});
        let open = if resume.is_some() {
            // Load the saved session; its history already holds the stage context, so
            // only the new message is sent below (R7).
            json!({"id":1,"method":"session/resume","params":{"sessionId":session,"workspace":workspace}})
        } else {
            let mut create = json!({"sessionId":session,"workspace":workspace,"titleGenerationEnabled":false,"mode":mode});
            if let Some(model) = model {
                create["model"] = model.clone();
            }
            json!({"id":1,"method":"session/create","params":create})
        };
        send(&mut input, open)?;
        for line in output.lines() {
            let line = line?;
            let msg: Value = serde_json::from_str(&line).context("无效的 ZCode 协议响应")?;
            if let Some(method) = msg["method"].as_str() {
                if !msg["id"].is_null() {
                    let answer = aep::zcode_catalog::client_response(method)?;
                    send(&mut input, json!({"id":msg["id"],"result":answer}))?;
                } else if method == "session/event" && msg["params"]["sessionId"] == session {
                    let e = &msg["params"];
                    match e["type"].as_str() {
                        Some("turn.failed") => bail!("ZCode: {}", e["payload"]["error"]),
                        Some("turn.completed") => {
                            let db = std::env::home_dir()
                                .map(|home| home.join(".zcode/cli/db/db.sqlite"));
                            let mut actual = app_server_receipt(
                                db.as_deref(),
                                &session,
                                &cwd,
                                model,
                                &selected,
                                &settings,
                            )?;
                            if resume.is_some() {
                                actual["resumed"] = json!(true);
                            }
                            save_receipt(dir, &actual)?;
                            send(
                                &mut input,
                                json!({"id":4,"method":"session/messages","params":{"sessionId":session,"limit":24}}),
                            )?;
                        }
                        Some("permission.requested" | "userInput.requested") => {
                            bail!("ZCode 等待工具权限或用户输入，请核对本机设置后重试")
                        }
                        Some("turn.started" | "tool.updated") => eprintln!("ZCode · {}", e["type"]),
                        _ => (),
                    }
                }
                continue;
            }
            if let Some(error) = msg["error"]["message"].as_str() {
                if resume.is_some() && msg["id"].as_u64() == Some(1) {
                    eprintln!("{RESUME_FAILED}: {error}");
                    bail!("{RESUME_FAILED}: {error}")
                }
                bail!("ZCode: {error}")
            }
            let result = &msg["result"];
            match msg["id"].as_u64() {
                Some(1) => {
                    settings = result["settings"]["model"]["current"].clone();
                    // A resumed snapshot may omit settings; the send below pins the model.
                    if resume.is_some()
                        && !settings["providerId"].is_string()
                        && let Some(model) = model
                    {
                        settings = model.clone();
                    }
                    if resume.is_none() || settings["providerId"].is_string() {
                        verify_model(
                            model,
                            &selected,
                            settings["providerId"].as_str().unwrap_or_default(),
                            settings["modelId"].as_str().unwrap_or_default(),
                            None,
                            false,
                        )?;
                    }
                    let mut native =
                        json!({"client":"ZCode","transport":"app-server","session":session});
                    if resume.is_some() {
                        native["resumed"] = json!(true);
                    }
                    aep::project::save_json(dir, "session.json", &native)
                        .context("无法写入 ZCode 实际模型回执")?;
                    send(
                        &mut input,
                        json!({"id":2,"method":"session/subscribe","params":{"sessionId":session,"deliveryKind":"desktop-continuous"}}),
                    )?;
                }
                Some(2) => {
                    let mut params = json!({"sessionId":session,"content":prompt});
                    if let Some(selection) = model {
                        params["modelSelection"] = selection.clone();
                    }
                    send(
                        &mut input,
                        json!({"id":3,"method":"session/send","params":params}),
                    )?;
                }
                Some(3) => ensure!(result["accepted"] == true, "ZCode 未接受请求"),
                Some(4) => return final_message(result),
                _ => (),
            }
        }
        bail!("ZCode 在返回交付物之前退出")
    }
}

fn app_server_receipt(
    db_path: Option<&Path>,
    session: &str,
    cwd: &Path,
    model: Option<&Value>,
    selected: &str,
    settings: &Value,
) -> Result<Value> {
    if let Some(path) = db_path
        && path.try_exists()?
    {
        let db = readonly(path)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(usage) = main_turn_usage(&db, session, None, model, selected)? {
                return Ok(
                    json!({"transport":"app-server","client":"ZCode","session":session,
                    "turn":usage.turn,"message":usage.message,"providerId":usage.provider,
                    "modelId":usage.model,"reasoningLevel":usage.variant,"workspace":cwd,
                    "selected":selected,"source":"model_usage"}),
                );
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100).min(remaining));
        }
    }
    let field = |name: &str| {
        settings[name]
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .context("缺少 ZCode 主对话模型用量回执，且会话设置未记录模型")
    };
    Ok(
        json!({"transport":"app-server","client":"ZCode","session":session,
        "turn":null,"message":null,"providerId":field("providerId")?,"modelId":field("modelId")?,
        "reasoningLevel":null,"workspace":cwd,"selected":selected,"source":"settings"}),
    )
}

fn send(out: &mut impl Write, v: Value) -> Result<()> {
    serde_json::to_writer(&mut *out, &v)?;
    writeln!(out)?;
    out.flush()?;
    Ok(())
}

fn final_message(reply: &Value) -> Result<String> {
    let messages = reply["messages"]
        .as_array()
        .context("ZCode 未返回消息列表")?;
    for msg in messages
        .iter()
        .rev()
        .filter(|m| m["info"]["role"] == "assistant" || m["role"] == "assistant")
    {
        let text = msg["parts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| p["type"] == "text")
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if !text.trim().is_empty() {
            return Ok(text);
        }
    }
    bail!("ZCode 未返回助手最终答复")
}

pub fn run(binary: &Path, request: &Path, resume: Option<&str>) -> Result<()> {
    let dir = request.parent().context("缺少执行目录")?;
    let req: Request = serde_json::from_reader(fs::File::open(request)?)?;
    let selection = (!req.profile.model.is_empty())
        .then(|| req.profile.zcode_selection())
        .transpose()?;
    let mode = match req.flow().stages.get(req.stage).context("未知阶段")?.role {
        Role::Build => "edit",
        Role::Test | Role::Accept => "guarded",
        _ => "plan",
    };
    let prompt = fs::read_to_string(dir.join("prompt.txt"))?;
    let reply = converse(
        binary,
        dir,
        mode,
        selection.as_ref(),
        &prompt,
        &std::env::current_dir()?,
        req.repository.as_deref(),
        resume,
    )?;
    println!("{reply}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A test double for `zcode app-server` that logs every request it reads.
    fn double(dir: &Path, resume_ok: bool) -> std::path::PathBuf {
        let bin = dir.join("zcode");
        let open = if resume_ok {
            r#"printf '%s\n' '{"id":1,"result":{"settings":{"model":{"current":{"providerId":"team","modelId":"qa"}}}}}'"#
        } else {
            r#"printf '%s\n' '{"id":1,"error":{"code":-32602,"message":"session not found"}}'; exit 0"#
        };
        fs::write(
            &bin,
            format!(
                r#"#!/bin/sh
set -eu
test "$1" = app-server
log="$(dirname "$0")/calls.log"
IFS= read -r open; printf '%s\n' "$open" >> "$log"
sid=$(printf '%s' "$open" | sed -n 's/.*"sessionId":"\([^"]*\)".*/\1/p')
{open}
IFS= read -r sub; printf '%s\n' "$sub" >> "$log"
printf '%s\n' '{{"id":2,"result":{{}}}}'
IFS= read -r send; printf '%s\n' "$send" >> "$log"
printf '%s\n' '{{"id":3,"result":{{"accepted":true}}}}'
printf '{{"method":"session/event","params":{{"sessionId":"%s","type":"turn.completed"}}}}\n' "$sid"
IFS= read -r msgs; printf '%s\n' "$msgs" >> "$log"
printf '%s\n' '{{"id":4,"result":{{"messages":[{{"info":{{"role":"assistant"}},"parts":[{{"type":"text","text":"continued"}}]}}]}}}}'
"#
            ),
        )
        .unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();
        bin
    }

    fn calls(dir: &Path) -> Vec<Value> {
        fs::read_to_string(dir.join("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn zcode_stage_resume_sends_only_the_new_message_to_the_saved_session() {
        let t = tempfile::tempdir().unwrap();
        let bin = double(t.path(), true);
        let model = json!({"providerId":"team","modelId":"qa"});
        let reply = app_server(
            &bin,
            t.path(),
            "edit",
            Some(&model),
            "只有这条新消息",
            Some("sess_old"),
        )
        .unwrap();
        assert_eq!(reply, "continued");
        let calls = calls(t.path());
        let methods: Vec<_> = calls.iter().map(|c| c["method"].clone()).collect();
        assert_eq!(
            methods,
            [
                "session/resume",
                "session/subscribe",
                "session/send",
                "session/messages"
            ]
        );
        // `session/resume` is strict: only the session and its workspace.
        let params = calls[0]["params"].as_object().unwrap();
        assert_eq!(params["sessionId"], "sess_old");
        assert!(params.keys().all(|k| k == "sessionId" || k == "workspace"));
        assert_eq!(calls[1]["params"]["sessionId"], "sess_old");
        assert_eq!(calls[2]["params"]["sessionId"], "sess_old");
        assert_eq!(calls[2]["params"]["content"], "只有这条新消息");
        let native: Value = aep::workspace::read(&t.path().join("session.json")).unwrap();
        assert_eq!(native["session"], "sess_old");
        assert_eq!(native["transport"], "app-server");
        assert_eq!(native["resumed"], true);
    }

    #[test]
    fn zcode_stage_failed_resume_reports_the_fallback_marker_and_sends_nothing() {
        let t = tempfile::tempdir().unwrap();
        let bin = double(t.path(), false);
        let error = app_server(&bin, t.path(), "plan", None, "新消息", Some("sess_gone"))
            .unwrap_err()
            .to_string();
        assert!(error.contains(RESUME_FAILED), "{error}");
        let calls = calls(t.path());
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0]["method"], "session/resume");
        assert!(!t.path().join("session.json").exists());
    }

    #[test]
    fn zcode_stage_without_resume_still_creates_a_session() {
        let t = tempfile::tempdir().unwrap();
        let bin = double(t.path(), true);
        let model = json!({"providerId":"team","modelId":"qa"});
        let reply = app_server(&bin, t.path(), "edit", Some(&model), "完整上下文", None).unwrap();
        assert_eq!(reply, "continued");
        let first = &calls(t.path())[0];
        assert_eq!(first["method"], "session/create");
        assert_eq!(first["params"]["mode"], "edit");
    }
}
