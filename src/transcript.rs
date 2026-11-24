//! Turn a stage CLI's raw stdout into a conversation a person can follow.
//!
//! Codex `exec --json` and Claude `--output-format stream-json` both write one JSON event
//! per line. Other clients (ZCode, custom CLIs) write plain text, which is kept as-is.
use serde_json::Value;
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// Assistant prose.
    Message(String),
    /// Model reasoning or thinking summary.
    Thinking(String),
    /// A shell command; `exit` is None while it is still running.
    Command {
        command: String,
        output: String,
        exit: Option<i64>,
    },
    /// Files the agent created, edited or removed.
    Edit(Vec<String>),
    /// Any other tool call (file reads, searches, MCP tools…).
    Tool {
        name: String,
        detail: String,
        output: String,
        failed: bool,
    },
    /// Errors and failed turns reported by the CLI.
    Error(String),
    /// Non-fatal CLI notices (Codex reports config warnings as `error` items).
    Notice(String),
    /// Unstructured output.
    Text(String),
}

#[derive(Default, Debug)]
pub struct Transcript {
    pub entries: Vec<Entry>,
    pub session: Option<String>,
    pub model: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str().or(p.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// One-line summary of a Claude tool input.
fn tool_detail(name: &str, input: &Value) -> String {
    let field = match name {
        "Read" | "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => "file_path",
        "Grep" | "Glob" => "pattern",
        "WebFetch" => "url",
        "WebSearch" => "query",
        "Task" | "Agent" => "description",
        _ => "",
    };
    input[field].as_str().map(str::to_owned).unwrap_or_else(|| {
        let s = input.to_string();
        if s == "{}" || s == "null" {
            String::new()
        } else {
            s.chars().take(160).collect()
        }
    })
}

/// Codex wraps API failures as a JSON string; keep only the human-readable message.
fn api_message(raw: &str) -> String {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|v| {
            v["error"]["message"]
                .as_str()
                .or(v["message"].as_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| raw.to_owned())
}

/// The fatal error a failed turn reported, if any.
pub fn failure(raw: &str) -> Option<String> {
    parse(raw).entries.into_iter().rev().find_map(|e| match e {
        Entry::Error(m) if !m.trim().is_empty() => Some(m),
        _ => None,
    })
}

pub fn parse(raw: &str) -> Transcript {
    let mut t = Transcript::default();
    // Codex item ID or Claude tool_use ID → index in `entries`, so completions update in place.
    let mut open: HashMap<String, usize> = HashMap::new();
    let mut plain = String::new();
    let flush = |plain: &mut String, entries: &mut Vec<Entry>| {
        if !plain.trim().is_empty() {
            entries.push(Entry::Text(plain.trim_end().to_owned()));
        }
        plain.clear();
    };
    for line in raw.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            plain.push_str(line);
            plain.push('\n');
            continue;
        };
        if !v.is_object() {
            plain.push_str(line);
            plain.push('\n');
            continue;
        }
        flush(&mut plain, &mut t.entries);
        match v["type"].as_str().unwrap_or_default() {
            // Codex
            "thread.started" => t.session = v["thread_id"].as_str().map(str::to_owned),
            "item.started" | "item.updated" | "item.completed" => codex_item(&mut t, &mut open, &v),
            "turn.completed" => {
                t.input_tokens += v["usage"]["input_tokens"].as_u64().unwrap_or(0);
                t.output_tokens += v["usage"]["output_tokens"].as_u64().unwrap_or(0);
            }
            // Codex reports a fatal error twice (`error`, then `turn.failed`); show it once.
            "turn.failed" | "error" => {
                let message = api_message(&text(if v["type"] == "error" {
                    &v["message"]
                } else {
                    &v["error"]["message"]
                }));
                if !matches!(t.entries.last(), Some(Entry::Error(last)) if *last == message) {
                    t.entries.push(Entry::Error(message));
                }
            }
            // Claude
            "system" if v["subtype"] == "init" => {
                t.session = v["session_id"].as_str().map(str::to_owned);
                t.model = v["model"].as_str().map(str::to_owned);
            }
            "assistant" => {
                for part in v["message"]["content"].as_array().into_iter().flatten() {
                    match part["type"].as_str() {
                        Some("text") => push_message(&mut t, text(&part["text"])),
                        Some("thinking") => {
                            let s = text(&part["thinking"]);
                            if !s.trim().is_empty() {
                                t.entries.push(Entry::Thinking(s));
                            }
                        }
                        Some("tool_use") => {
                            let name = part["name"].as_str().unwrap_or("tool").to_owned();
                            let input = &part["input"];
                            let entry = match name.as_str() {
                                "Bash" => Entry::Command {
                                    command: text(&input["command"]),
                                    output: String::new(),
                                    exit: None,
                                },
                                "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => {
                                    Entry::Edit(vec![tool_detail(&name, input)])
                                }
                                _ => Entry::Tool {
                                    detail: tool_detail(&name, input),
                                    name,
                                    output: String::new(),
                                    failed: false,
                                },
                            };
                            if let Some(id) = part["id"].as_str() {
                                open.insert(id.to_owned(), t.entries.len());
                            }
                            t.entries.push(entry);
                        }
                        _ => {}
                    }
                }
            }
            "user" => {
                for part in v["message"]["content"].as_array().into_iter().flatten() {
                    if part["type"] != "tool_result" {
                        continue;
                    }
                    let Some(&i) = part["tool_use_id"].as_str().and_then(|id| open.get(id)) else {
                        continue;
                    };
                    let failed = part["is_error"] == true;
                    match &mut t.entries[i] {
                        Entry::Command { output, exit, .. } => {
                            *output = text(&part["content"]);
                            *exit = Some(i64::from(failed));
                        }
                        Entry::Tool {
                            output, failed: f, ..
                        } => {
                            *output = text(&part["content"]);
                            *f = failed;
                        }
                        _ => {}
                    }
                }
            }
            "result" | "" if !v["result"].is_null() => {
                if let Some(s) = v["session_id"].as_str() {
                    t.session = Some(s.to_owned());
                }
                t.input_tokens += v["usage"]["input_tokens"].as_u64().unwrap_or(0);
                t.output_tokens += v["usage"]["output_tokens"].as_u64().unwrap_or(0);
                if v["is_error"] == true {
                    t.entries.push(Entry::Error(text(&v["result"])));
                } else if !t.entries.iter().any(|e| matches!(e, Entry::Message(_))) {
                    // Single-object JSON output carries only the final answer.
                    push_message(&mut t, text(&v["result"]));
                }
            }
            _ => {}
        }
    }
    flush(&mut plain, &mut t.entries);
    t
}

fn push_message(t: &mut Transcript, s: String) {
    if !s.trim().is_empty() {
        t.entries.push(Entry::Message(s));
    }
}

fn codex_item(t: &mut Transcript, open: &mut HashMap<String, usize>, v: &Value) {
    let item = &v["item"];
    let done = v["type"] == "item.completed";
    let entry = match item["type"].as_str().unwrap_or_default() {
        "agent_message" => Entry::Message(text(&item["text"])),
        "reasoning" => Entry::Thinking(text(&item["text"])),
        "command_execution" => Entry::Command {
            command: text(&item["command"]),
            output: text(&item["aggregated_output"]),
            exit: item["exit_code"].as_i64().filter(|_| done),
        },
        "file_change" => Entry::Edit(
            item["changes"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| {
                    let path = c["path"].as_str().unwrap_or_default();
                    match c["kind"].as_str() {
                        Some("add") => format!("+ {path}"),
                        Some("delete") => format!("- {path}"),
                        _ => path.to_owned(),
                    }
                })
                .collect(),
        ),
        "mcp_tool_call" => Entry::Tool {
            name: format!(
                "{}.{}",
                item["server"].as_str().unwrap_or("mcp"),
                item["tool"].as_str().unwrap_or("tool")
            ),
            detail: String::new(),
            output: String::new(),
            failed: item["status"] == "failed",
        },
        "web_search" => Entry::Tool {
            name: "web_search".into(),
            detail: text(&item["query"]),
            output: String::new(),
            failed: false,
        },
        "todo_list" => Entry::Tool {
            name: "计划".into(),
            detail: String::new(),
            output: item["items"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|i| {
                    format!(
                        "{} {}",
                        if i["completed"] == true { "☑" } else { "☐" },
                        text(&i["text"])
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
            failed: false,
        },
        // Item-level errors are warnings; a failing turn arrives as `turn.failed`.
        "error" => {
            let notice = text(&item["message"]);
            if t.entries
                .iter()
                .any(|e| matches!(e, Entry::Notice(s) if *s == notice))
            {
                return;
            }
            Entry::Notice(notice)
        }
        _ => return,
    };
    if matches!(&entry, Entry::Message(s) | Entry::Thinking(s) if s.trim().is_empty()) {
        return;
    }
    let id = item["id"].as_str().unwrap_or_default().to_owned();
    match open.get(&id) {
        Some(&i) if !id.is_empty() => t.entries[i] = entry,
        _ => {
            if !id.is_empty() {
                open.insert(id, t.entries.len());
            }
            t.entries.push(entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_items_update_in_place() {
        let raw = r#"{"type":"thread.started","thread_id":"t-1"}
{"type":"item.started","item":{"id":"i1","type":"command_execution","command":"ls","aggregated_output":"","status":"in_progress"}}
{"type":"item.completed","item":{"id":"i1","type":"command_execution","command":"ls","aggregated_output":"a.txt\n","exit_code":0,"status":"completed"}}
{"type":"item.completed","item":{"id":"i2","type":"file_change","changes":[{"path":"src/a.rs","kind":"update"},{"path":"b.rs","kind":"add"}]}}
{"type":"item.completed","item":{"id":"i3","type":"agent_message","text":"done"}}
{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":3}}"#;
        let t = parse(raw);
        assert_eq!(t.session.as_deref(), Some("t-1"));
        assert_eq!(
            t.entries,
            vec![
                Entry::Command {
                    command: "ls".into(),
                    output: "a.txt\n".into(),
                    exit: Some(0)
                },
                Entry::Edit(vec!["src/a.rs".into(), "+ b.rs".into()]),
                Entry::Message("done".into()),
            ]
        );
        assert_eq!((t.input_tokens, t.output_tokens), (10, 3));
    }

    #[test]
    fn codex_item_errors_are_deduplicated_notices() {
        let raw = r#"{"type":"item.completed","item":{"id":"e0","type":"error","message":"hooks enabled"}}
{"type":"item.completed","item":{"id":"e1","type":"error","message":"hooks enabled"}}
{"type":"turn.failed","error":{"message":"model not found"}}"#;
        assert_eq!(
            parse(raw).entries,
            vec![
                Entry::Notice("hooks enabled".into()),
                Entry::Error("model not found".into())
            ]
        );
    }

    #[test]
    fn claude_tool_results_attach_to_their_calls() {
        let raw = r#"{"type":"system","subtype":"init","session_id":"s-1","model":"sonnet"}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"u1","name":"Bash","input":{"command":"ls"}}]}}
{"type":"user","message":{"content":[{"tool_use_id":"u1","type":"tool_result","content":"a.txt","is_error":false}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"u2","name":"Read","input":{"file_path":"/x/a.rs"}}]}}
{"type":"assistant","message":{"content":[{"type":"text","text":"done"}]}}
{"type":"result","subtype":"success","result":"done","is_error":false,"session_id":"s-1"}"#;
        let t = parse(raw);
        assert_eq!(t.session.as_deref(), Some("s-1"));
        assert_eq!(t.model.as_deref(), Some("sonnet"));
        assert_eq!(t.entries.len(), 3);
        assert_eq!(
            t.entries[0],
            Entry::Command {
                command: "ls".into(),
                output: "a.txt".into(),
                exit: Some(0)
            }
        );
        assert!(
            matches!(&t.entries[1], Entry::Tool { name, detail, .. } if name == "Read" && detail == "/x/a.rs")
        );
        assert_eq!(t.entries[2], Entry::Message("done".into()));
    }

    #[test]
    fn plain_and_single_object_output_are_kept() {
        let t = parse("hello\nworld\n");
        assert_eq!(t.entries, vec![Entry::Text("hello\nworld".into())]);
        let t = parse(r#"{"result":"final answer","is_error":false}"#);
        assert_eq!(t.entries, vec![Entry::Message("final answer".into())]);
    }

    #[test]
    fn api_failures_read_as_their_message() {
        let raw = r#"{"type":"error","message":"{\"type\":\"error\",\"status\":400,\"error\":{\"message\":\"model is not supported\"}}"}
{"type":"turn.failed","error":{"message":"{\"type\":\"error\",\"status\":400,\"error\":{\"message\":\"model is not supported\"}}"}}"#;
        assert_eq!(
            parse(raw).entries,
            vec![Entry::Error("model is not supported".into())]
        );
        assert_eq!(failure(raw).as_deref(), Some("model is not supported"));
        assert_eq!(failure(r#"{"type":"turn.started"}"#), None);
    }
}
