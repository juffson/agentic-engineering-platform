//! Use the installed ZCode app-server with its native settings or a pinned model.
use aep::workflow::{Request, Role};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::Stdio,
};

/// Run a configured desktop plan in its native task, or an ordinary CLI model via app-server.
pub fn converse(
    binary: &Path,
    dir: &Path,
    mode: &str,
    model: Option<&Value>,
    prompt: &str,
    desktop_workspace: &Path,
) -> Result<String> {
    if Path::new(aep::workflow::ZCODE_APP).is_file()
        && let Some(model) = model
        && model["providerId"]
            .as_str()
            .and_then(aep::zcode_catalog::desktop_plan)
            .is_some()
    {
        let selection = json!({"providerId":model["providerId"], "modelId":model["modelId"]});
        return crate::zcode_desktop::converse(dir, desktop_workspace, mode, &selection, prompt)
            .inspect_err(|e| {
                let _ = aep::project::save_json(
                    dir,
                    "desktop-error.json",
                    &json!({"error":format!("{e:#}")}),
                );
            });
    }
    let mut child = aep::zcode_catalog::app_server(binary)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("启动 ZCode app-server")?;
    // The outer supervisor owns this process group and its deadline/cancel marker.
    let talk = (|| -> Result<String> {
        let mut input = child.stdin.take().context("ZCode stdin")?;
        let output = BufReader::new(child.stdout.take().context("ZCode stdout")?);
        let cwd = std::env::current_dir()?;
        let session = format!("sess_{}", uuid::Uuid::new_v4().simple());
        let mut create = json!({"sessionId":session,"workspace":{"workspaceKey":cwd,"workspacePath":cwd},"titleGenerationEnabled":false,"mode":mode});
        if let Some(model) = model {
            create["model"] = model.clone();
        }
        send(
            &mut input,
            json!({"id":1,"method":"session/create","params":create}),
        )?;
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
                        Some("turn.completed") => send(
                            &mut input,
                            json!({"id":4,"method":"session/messages","params":{"sessionId":session,"limit":24}}),
                        )?,
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
                bail!("ZCode: {error}")
            }
            let result = &msg["result"];
            match msg["id"].as_u64() {
                Some(1) => {
                    let actual = &result["settings"]["model"]["current"];
                    if let Some(model) = &model {
                        ensure!(
                            actual["providerId"] == model["providerId"]
                                && actual["modelId"] == model["modelId"],
                            "ZCode 未选中指定模型"
                        );
                    }
                    aep::project::save_json(
                        dir,
                        "session.json",
                        &json!({"client":"ZCode","transport":"app-server","session":session}),
                    )?;
                    fs::write(
                        dir.join("zcode-model.json"),
                        serde_json::to_vec_pretty(actual)?,
                    )?;
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
    })();
    let _ = child.kill();
    let _ = child.wait();
    talk
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

pub fn run(binary: &Path, request: &Path) -> Result<()> {
    let dir = request.parent().context("缺少执行目录")?;
    let req: Request = serde_json::from_reader(fs::File::open(request)?)?;
    let selection = (!req.profile.model.is_empty())
        .then(|| req.profile.zcode_selection())
        .transpose()?;
    let mode = match req
        .template
        .stages()
        .get(req.stage)
        .context("未知阶段")?
        .role
    {
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
    )?;
    println!("{reply}");
    Ok(())
}
