//! Token usage of stage runs, read from what each CLI itself reports: Claude's stream-json
//! (per API call and the final `result`), Codex's `turn.completed` and its session rollout
//! (`token_count` per model call), and ZCode's `model_usage` table. Nothing is estimated
//! except the size of tool outputs, which explains where context grew.
use crate::workflow::Client;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

/// Token counts with cached input kept apart from fresh input, the same way for every CLI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    /// Input not served from cache.
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// Output including reasoning.
    pub output: u64,
    /// The reasoning part of `output`, when the CLI reports it.
    pub reasoning: u64,
}

impl Tokens {
    pub fn total(&self) -> u64 {
        self.input + self.cache_read + self.cache_write + self.output
    }
    /// Everything the model read for this call: its context size.
    pub fn context(&self) -> u64 {
        self.input + self.cache_read + self.cache_write
    }
    pub fn add(&mut self, o: &Tokens) {
        self.input += o.input;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
        self.output += o.output;
        self.reasoning += o.reasoning;
    }
}

/// One model call and what the model did with it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Call {
    pub tokens: Tokens,
    /// 读取 / 命令 / 编辑 / 其他工具 / 回答.
    pub kind: String,
    pub detail: String,
    /// Bytes of tool output this call's tools returned (they stay in context from here on).
    pub output_bytes: u64,
}

/// What a run used: the cached summary kept in `usage.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub tokens: Tokens,
    /// Only Claude reports a cost.
    pub cost_usd: Option<f64>,
    pub models: Vec<String>,
    pub calls: usize,
    /// Account quota the CLI reported last, e.g. {"five_hour":0.41,"seven_day":0.33}.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota: Option<Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Report {
    #[serde(flatten)]
    pub summary: Summary,
    pub per_call: Vec<Call>,
}

fn n(v: &Value) -> u64 {
    v.as_u64().unwrap_or(0)
}

fn kind_of_claude_tool(name: &str) -> &'static str {
    match name {
        "Read" | "Grep" | "Glob" | "LS" => "读取",
        "Bash" => "命令",
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => "编辑",
        _ => "其他工具",
    }
}

fn short(s: &str, max: usize) -> String {
    let line = s
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let mut out: String = line.chars().take(max).collect();
    if line.chars().count() > max {
        out.push('…');
    }
    out
}

fn content_bytes(v: &Value) -> u64 {
    match v {
        Value::String(s) => s.len() as u64,
        Value::Array(parts) => parts
            .iter()
            .map(|p| {
                p["text"]
                    .as_str()
                    .map_or_else(|| content_bytes(p), |t| t.len() as u64)
            })
            .sum(),
        Value::Null => 0,
        other => other.to_string().len() as u64,
    }
}

fn claude_tokens(u: &Value) -> Tokens {
    Tokens {
        input: n(&u["input_tokens"]),
        cache_read: n(&u["cache_read_input_tokens"]),
        cache_write: n(&u["cache_creation_input_tokens"]),
        output: n(&u["output_tokens"]),
        reasoning: n(&u["output_tokens_details"]["thinking_tokens"]),
    }
}

/// Claude `--output-format stream-json`: every assistant event carries its API call's usage
/// (repeated for each content block of the same message); `result` has the run's totals.
pub fn claude(raw: &str) -> Report {
    let mut calls: Vec<Call> = Vec::new();
    let mut by_message: HashMap<String, usize> = HashMap::new();
    let mut by_tool: HashMap<String, usize> = HashMap::new();
    let mut result: Option<Value> = None;
    let mut models: Vec<String> = Vec::new();
    let mut quota = None;
    for line in raw.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match v["type"].as_str().unwrap_or_default() {
            "system" if v["subtype"] == "init" => {
                if let Some(m) = v["model"].as_str()
                    && !models.iter().any(|x| x == m)
                {
                    models.push(m.to_owned());
                }
            }
            "assistant" => {
                let msg = &v["message"];
                let id = msg["id"].as_str().unwrap_or_default().to_owned();
                let i = *by_message.entry(id).or_insert_with(|| {
                    calls.push(Call {
                        kind: "回答".into(),
                        ..Call::default()
                    });
                    calls.len() - 1
                });
                if msg["usage"].is_object() {
                    calls[i].tokens = claude_tokens(&msg["usage"]);
                }
                for part in msg["content"].as_array().into_iter().flatten() {
                    if part["type"] != "tool_use" {
                        continue;
                    }
                    let name = part["name"].as_str().unwrap_or("tool");
                    let input = &part["input"];
                    let what = ["file_path", "pattern", "command", "url", "description"]
                        .iter()
                        .find_map(|k| input[k].as_str())
                        .unwrap_or("");
                    let call = &mut calls[i];
                    let kind = kind_of_claude_tool(name);
                    // A call that both reads and edits counts as its strongest action.
                    if call.kind == "回答" || kind == "编辑" {
                        call.kind = kind.into();
                    }
                    if call.detail.is_empty() {
                        call.detail = format!("{name} {}", short(what, 80)).trim().to_owned();
                    }
                    if let Some(tid) = part["id"].as_str() {
                        by_tool.insert(tid.to_owned(), i);
                    }
                }
            }
            "user" => {
                for part in v["message"]["content"].as_array().into_iter().flatten() {
                    if part["type"] == "tool_result"
                        && let Some(&i) = part["tool_use_id"].as_str().and_then(|t| by_tool.get(t))
                    {
                        calls[i].output_bytes += content_bytes(&part["content"]);
                    }
                }
            }
            "rate_limit_event" => {
                let w = &v["rate_limit_info"]["unifiedWindows"];
                if let Some(o) = w.as_object() {
                    let map: serde_json::Map<String, Value> = o
                        .iter()
                        .filter_map(|(k, x)| Some((k.clone(), x["utilization"].as_f64()?.into())))
                        .collect();
                    if !map.is_empty() {
                        quota = Some(Value::Object(map));
                    }
                }
            }
            "result" => result = Some(v),
            _ => {}
        }
    }
    let mut summary = Summary {
        calls: calls.len(),
        models,
        quota,
        ..Summary::default()
    };
    match &result {
        Some(r) if r["usage"].is_object() => {
            summary.tokens = claude_tokens(&r["usage"]);
            summary.cost_usd = r["total_cost_usd"].as_f64();
            if let Some(m) = r["modelUsage"].as_object() {
                summary.models = m.keys().cloned().collect();
            }
        }
        _ => calls.iter().for_each(|c| summary.tokens.add(&c.tokens)),
    }
    Report {
        summary,
        per_call: calls,
    }
}

fn codex_tokens(u: &Value) -> Tokens {
    let input = n(&u["input_tokens"]);
    let cached = n(&u["cached_input_tokens"]);
    let write = n(&u["cache_write_input_tokens"]);
    Tokens {
        input: input.saturating_sub(cached + write),
        cache_read: cached,
        cache_write: write,
        output: n(&u["output_tokens"]),
        reasoning: n(&u["reasoning_output_tokens"]),
    }
}

fn codex_kind(input: &str) -> &'static str {
    if input.contains("apply_patch") {
        return "编辑";
    }
    let cmd = input
        .split("cmd:")
        .nth(1)
        .or_else(|| input.split("\"command\"").nth(1))
        .unwrap_or(input)
        .trim_start_matches([' ', '"', '[', ':', '\'', '`']);
    let first = cmd.split_whitespace().next().unwrap_or("");
    match first.trim_matches(['"', '\'', ',']) {
        "cat" | "sed" | "rg" | "grep" | "ls" | "find" | "head" | "tail" | "nl" | "wc" | "git"
        | "codegraph" | "fd" | "tree" | "pwd" | "jq" => "读取",
        _ => "命令",
    }
}

/// Codex `exec --json` reports usage once per turn; its rollout file has every model call
/// (`token_count.last_token_usage`) and the tool calls that came before it.
pub fn codex(raw: &str, rollout: Option<&str>, window: (Option<&str>, Option<&str>)) -> Report {
    let mut summary = Summary::default();
    let mut turns = 0;
    for line in raw.lines() {
        if let Ok(v) = serde_json::from_str::<Value>(line)
            && v["type"] == "turn.completed"
        {
            summary.tokens.add(&codex_tokens(&v["usage"]));
            turns += 1;
        }
    }
    let mut calls = Vec::new();
    let mut pending = Call {
        kind: "回答".into(),
        ..Call::default()
    };
    let mut open: HashMap<String, bool> = HashMap::new();
    let in_window = |ts: &str| window.0.is_none_or(|a| ts >= a) && window.1.is_none_or(|b| ts <= b);
    for line in rollout.unwrap_or("").lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let ts = v["timestamp"].as_str().unwrap_or("");
        if !in_window(ts) {
            continue;
        }
        let p = &v["payload"];
        match (v["type"].as_str(), p["type"].as_str()) {
            (Some("session_meta"), _) => {}
            (Some("turn_context"), _) => {
                if let Some(m) = p["model"].as_str()
                    && !summary.models.iter().any(|x| x == m)
                {
                    summary.models.push(m.to_owned());
                }
            }
            (Some("response_item"), Some("function_call" | "custom_tool_call")) => {
                let input = p["input"]
                    .as_str()
                    .or(p["arguments"].as_str())
                    .unwrap_or("");
                let kind = codex_kind(input);
                if pending.kind == "回答" || kind == "编辑" {
                    pending.kind = kind.into();
                }
                if pending.detail.is_empty() {
                    let cmd = match input.split("cmd:").nth(1) {
                        Some(rest) => {
                            let rest = rest.trim_start();
                            let quote = rest
                                .chars()
                                .next()
                                .filter(|c| matches!(c, '"' | '`' | '\''));
                            match quote {
                                Some(q) => rest[1..].split(q).next().unwrap_or(""),
                                None => rest,
                            }
                        }
                        None => input,
                    };
                    pending.detail = short(cmd, 80);
                }
                if let Some(id) = p["call_id"].as_str() {
                    open.insert(id.to_owned(), true);
                }
            }
            (Some("response_item"), Some("function_call_output" | "custom_tool_call_output")) => {
                // The output arrives after the call that asked for it; it is read by the next.
                let bytes = content_bytes(&p["output"]);
                if let Some(last) = calls.last_mut().filter(|_: &&mut Call| {
                    p["call_id"]
                        .as_str()
                        .is_some_and(|id| open.remove(id).is_some())
                }) {
                    last.output_bytes += bytes;
                } else {
                    pending.output_bytes += bytes;
                }
            }
            (Some("event_msg"), Some("token_count")) => {
                let info = &p["info"];
                if !info["last_token_usage"].is_object() {
                    continue;
                }
                let mut call = std::mem::replace(
                    &mut pending,
                    Call {
                        kind: "回答".into(),
                        ..Call::default()
                    },
                );
                call.tokens = codex_tokens(&info["last_token_usage"]);
                calls.push(call);
                if let Some(primary) = p["rate_limits"]["primary"].as_object()
                    && let Some(pct) = primary.get("used_percent").and_then(Value::as_f64)
                {
                    let window = primary
                        .get("window_minutes")
                        .and_then(Value::as_u64)
                        .map_or("primary".to_owned(), |m| {
                            if m >= 10080 {
                                "seven_day".into()
                            } else if m >= 300 {
                                "five_hour".into()
                            } else {
                                format!("{m}m")
                            }
                        });
                    summary.quota = Some(json!({ window: pct / 100.0 }));
                }
            }
            _ => {}
        }
    }
    // A run stopped mid-turn never wrote `turn.completed`: its calls are all there is.
    if turns == 0 {
        calls.iter().for_each(|c| summary.tokens.add(&c.tokens));
    }
    summary.calls = calls.len();
    Report {
        summary,
        per_call: calls,
    }
}

/// ZCode's own accounting: every model request of the session in the run's time window.
pub fn zcode(db: &Path, session: &str, since_ms: i64, until_ms: Option<i64>) -> Result<Report> {
    let db = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut q = db.prepare(
        "SELECT model_id, query_source, tool_call_count, input_tokens, cache_read_input_tokens,
                cache_creation_input_tokens, output_tokens, reasoning_tokens
         FROM model_usage WHERE session_id=?1 AND started_at>=?2 AND (?3 IS NULL OR started_at<=?3)
         ORDER BY started_at",
    )?;
    let mut report = Report::default();
    let rows = q.query_map(rusqlite::params![session, since_ms, until_ms], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            [
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)?,
            ],
        ))
    })?;
    for row in rows {
        let (model, source, tools, [input, read, write, output, reasoning]) = row?;
        let u = |x: i64| x.max(0) as u64;
        let tokens = Tokens {
            // ZCode's input includes the cached part.
            input: u(input).saturating_sub(u(read) + u(write)),
            cache_read: u(read),
            cache_write: u(write),
            output: u(output),
            reasoning: u(reasoning),
        };
        report.summary.tokens.add(&tokens);
        if !report.summary.models.contains(&model) {
            report.summary.models.push(model);
        }
        report.per_call.push(Call {
            tokens,
            kind: if source != "main_turn" {
                "其他工具".into()
            } else if tools > 0 {
                "命令".into()
            } else {
                "回答".into()
            },
            detail: if tools > 0 {
                format!("{source} · {tools} 个工具调用")
            } else {
                source
            },
            output_bytes: 0,
        });
    }
    report.summary.calls = report.per_call.len();
    Ok(report)
}

fn codex_home() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|h| h.join(".codex")))
}

/// The rollout file of a Codex thread, remembered in the attempt folder once found.
fn rollout(dir: &Path, thread: &str) -> Option<PathBuf> {
    let memo = dir.join("rollout.path");
    if let Ok(p) = fs::read_to_string(&memo)
        && Path::new(p.trim()).is_file()
    {
        return Some(PathBuf::from(p.trim()));
    }
    let sessions = codex_home()?.join("sessions");
    let mut stack = vec![sessions];
    // Newest folders first: sessions are filed by date.
    while let Some(d) = stack.pop() {
        let mut entries: Vec<_> = fs::read_dir(&d).ok()?.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                stack.push(p);
            } else if p
                .file_name()
                .and_then(|f| f.to_str())
                .is_some_and(|f| f.ends_with(&format!("{thread}.jsonl")))
            {
                let _ = fs::write(&memo, p.display().to_string());
                return Some(p);
            }
        }
    }
    None
}

fn first_event(stdout: &Path, kind: &str) -> Option<Value> {
    let f = fs::File::open(stdout).ok()?;
    BufReader::new(f)
        .lines()
        .take(40)
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str::<Value>(&l).ok())
        .find(|v| v["type"] == kind)
}

fn millis(ts: &Value) -> Option<i64> {
    Some(
        chrono::DateTime::parse_from_rfc3339(ts.as_str()?)
            .ok()?
            .timestamp_millis(),
    )
}

fn zcode_session(dir: &Path) -> Option<String> {
    ["", "zcode-edit", "zcode-plan"].iter().find_map(|sub| {
        let d = dir.join(sub);
        [
            "session.json",
            "desktop-task.json",
            "desktop-approvals.json",
        ]
        .iter()
        .find_map(|f| {
            let v = crate::workspace::read(&d.join(f)).ok()?;
            v["session"].as_str().map(str::to_owned)
        })
    })
}

/// Measure an attempt from its logs (works while it is still running).
pub fn measure(dir: &Path) -> Result<Report> {
    let ev = crate::workflow::evidence(dir)?;
    let client: Option<Client> = serde_json::from_value(ev["profile"]["client"].clone()).ok();
    let stdout = dir.join("stdout.log");
    let raw = || fs::read_to_string(&stdout).unwrap_or_default();
    Ok(match client {
        Some(Client::Claude) => claude(&raw()),
        Some(Client::Codex) => {
            let text = raw();
            let rollout = first_event(&stdout, "thread.started")
                .and_then(|v| v["thread_id"].as_str().map(str::to_owned))
                .or_else(|| ev["session"].as_str().map(str::to_owned))
                .and_then(|t| rollout(dir, &t))
                .and_then(|p| fs::read_to_string(p).ok());
            // Rollout timestamps are UTC with a `Z`; compare in the same form.
            let utc = |v: &Value| {
                v.as_str()
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|t| {
                        t.with_timezone(&chrono::Utc)
                            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                            .to_string()
                    })
            };
            let (a, b) = (utc(&ev["started_at"]), utc(&ev["finished_at"]));
            codex(&text, rollout.as_deref(), (a.as_deref(), b.as_deref()))
        }
        Some(Client::ZCode) => {
            let db = std::env::home_dir()
                .map(|h| h.join(".zcode/cli/db/db.sqlite"))
                .filter(|p| p.is_file());
            match (db, zcode_session(dir), millis(&ev["started_at"])) {
                (Some(db), Some(session), Some(since)) => {
                    zcode(&db, &session, since, millis(&ev["finished_at"]))?
                }
                _ => Report::default(),
            }
        }
        _ => Report::default(),
    })
}

fn finished(ev: &Value) -> bool {
    !matches!(
        ev["status"].as_str(),
        None | Some("running" | "initializing" | "claimed")
    )
}

/// An attempt's usage, cached in `usage.json` once the attempt has finished.
pub fn summary(dir: &Path) -> Summary {
    let cache = dir.join("usage.json");
    if let Ok(bytes) = fs::read(&cache)
        && let Ok(s) = serde_json::from_slice::<Summary>(&bytes)
    {
        return s;
    }
    let Ok(ev) = crate::workflow::evidence(dir) else {
        return Summary::default();
    };
    let s = measure(dir).map(|r| r.summary).unwrap_or_default();
    if finished(&ev) {
        let _ = crate::project::save_json(dir, "usage.json", &s);
    }
    s
}

/// Tokens used against the budgets of one stage and of the whole item.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Budget {
    pub stage_used: u64,
    pub stage_limit: Option<u64>,
    pub total_used: u64,
    pub total_limit: Option<u64>,
}

pub fn budget(
    catalog: &Path,
    project: &crate::project::Project,
    work: &crate::project::Work,
    stage: usize,
) -> Budget {
    let mut b = Budget {
        stage_limit: work.budgets.get(&stage).copied().or(project.stage_budget),
        total_limit: work.budget_total,
        ..Budget::default()
    };
    for a in &work.attempts {
        let dir = crate::workflow::attempt_dir(catalog, &project.id, &work.id, &a.id);
        let used = summary(&dir).tokens.total();
        b.total_used += used;
        if a.stage == stage {
            b.stage_used += used;
        }
    }
    b
}

/// Compact token count for messages: 1.2M, 35K.
pub fn compact(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{}K", n / 1_000)
    } else {
        n.to_string()
    }
}

impl Budget {
    /// What the next attempt may use; None when nothing limits it.
    pub fn remaining(&self) -> Option<u64> {
        let stage = self.stage_limit.map(|l| l.saturating_sub(self.stage_used));
        let total = self.total_limit.map(|l| l.saturating_sub(self.total_used));
        match (stage, total) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
    /// Why no attempt may start, when a budget is used up.
    pub fn exhausted(&self) -> Option<String> {
        if let Some(l) = self.total_limit
            && self.total_used >= l
        {
            return Some(format!(
                "事项 Token 总预算已用完（已用 {} / 上限 {}），请追加预算",
                compact(self.total_used),
                compact(l)
            ));
        }
        if let Some(l) = self.stage_limit
            && self.stage_used >= l
        {
            return Some(format!(
                "此阶段 Token 预算已用完（已用 {} / 上限 {}），请追加预算",
                compact(self.stage_used),
                compact(l)
            ));
        }
        None
    }
}

/// Where a run's tokens went: totals per kind of step, the context size of every call, and
/// the tool outputs that made the context grow most (each is read again by every later call).
pub fn analysis(report: &Report) -> Value {
    let mut kinds: Vec<(String, usize, Tokens, u64)> = Vec::new();
    for c in &report.per_call {
        match kinds.iter_mut().find(|k| k.0 == c.kind) {
            Some(k) => {
                k.1 += 1;
                k.2.add(&c.tokens);
                k.3 += c.output_bytes;
            }
            None => kinds.push((c.kind.clone(), 1, c.tokens, c.output_bytes)),
        }
    }
    kinds.sort_by_key(|k| std::cmp::Reverse(k.2.total()));
    let calls = &report.per_call;
    let total = calls.len();
    // How much a step added to the context, measured by the next call's context size: its
    // tool output and its own reply. Every later call reads it again.
    let mut heavy: Vec<Value> = (0..total.saturating_sub(1))
        .filter_map(|i| {
            let (c, next) = (&calls[i], &calls[i + 1]);
            let growth = next.tokens.context().checked_sub(c.tokens.context())?;
            (growth >= 1000).then(|| {
                json!({"call": i, "kind": c.kind, "detail": c.detail, "bytes": c.output_bytes,
                       "tokens": growth, "carried": growth * (total - i - 1) as u64})
            })
        })
        .collect();
    heavy.sort_by_key(|v| std::cmp::Reverse(v["carried"].as_u64().unwrap_or(0)));
    heavy.truncate(8);
    json!({
        "by_kind": kinds.iter().map(|(k, calls, t, bytes)| json!({"kind": k, "calls": calls, "tokens": t, "total": t.total(), "output_bytes": bytes})).collect::<Vec<_>>(),
        "calls": report.per_call.iter().map(|c| json!({"kind": c.kind, "detail": c.detail, "context": c.tokens.context(), "output": c.tokens.output})).collect::<Vec<_>>(),
        "heaviest": heavy,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remaining_is_the_tighter_budget() {
        let b = Budget {
            stage_used: 30,
            stage_limit: Some(100),
            total_used: 90,
            total_limit: Some(100),
        };
        assert_eq!(b.remaining(), Some(10));
        assert!(b.exhausted().is_none());
        let b = Budget {
            stage_used: 100,
            stage_limit: Some(100),
            ..Budget::default()
        };
        assert_eq!(b.remaining(), Some(0));
        assert!(b.exhausted().unwrap().contains("此阶段"));
        assert_eq!(Budget::default().remaining(), None);
        assert_eq!(compact(1_250_000), "1.2M");
    }

    #[test]
    fn claude_counts_each_call_once_and_trusts_the_result() {
        let raw = r#"{"type":"system","subtype":"init","model":"claude-x"}
{"type":"assistant","message":{"id":"m1","usage":{"input_tokens":5,"cache_creation_input_tokens":100,"cache_read_input_tokens":0,"output_tokens":3},"content":[{"type":"text","text":"看看"}]}}
{"type":"assistant","message":{"id":"m1","usage":{"input_tokens":5,"cache_creation_input_tokens":100,"cache_read_input_tokens":0,"output_tokens":9},"content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"src/a.rs"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"0123456789"}]}}
{"type":"assistant","message":{"id":"m2","usage":{"input_tokens":1,"cache_creation_input_tokens":10,"cache_read_input_tokens":100,"output_tokens":4},"content":[{"type":"tool_use","id":"t2","name":"Edit","input":{"file_path":"src/a.rs"}}]}}
{"type":"rate_limit_event","rate_limit_info":{"unifiedWindows":{"five_hour":{"utilization":0.4}}}}"#;
        let r = claude(raw);
        assert_eq!(r.per_call.len(), 2);
        assert_eq!(r.per_call[0].kind, "读取");
        assert_eq!(r.per_call[0].detail, "Read src/a.rs");
        assert_eq!(r.per_call[0].output_bytes, 10);
        assert_eq!(r.per_call[1].kind, "编辑");
        // Without a result the calls are summed.
        assert_eq!(r.summary.tokens.total(), 5 + 100 + 9 + 1 + 10 + 100 + 4);
        assert_eq!(r.summary.quota, Some(json!({"five_hour": 0.4})));
        let done = format!(
            "{raw}\n{}",
            r#"{"type":"result","result":"ok","total_cost_usd":0.5,"usage":{"input_tokens":6,"cache_creation_input_tokens":110,"cache_read_input_tokens":100,"output_tokens":13},"modelUsage":{"claude-x":{}}}"#
        );
        let r = claude(&done);
        assert_eq!(r.summary.tokens.total(), 229);
        assert_eq!(r.summary.cost_usd, Some(0.5));
        assert_eq!(r.summary.models, vec!["claude-x".to_owned()]);
    }

    #[test]
    fn codex_splits_cached_input_and_reads_calls_from_the_rollout() {
        let stdout = r#"{"type":"thread.started","thread_id":"t"}
{"type":"turn.completed","usage":{"input_tokens":1000,"cached_input_tokens":800,"output_tokens":50,"reasoning_output_tokens":20}}"#;
        let rollout = r#"{"timestamp":"2026-01-01T00:00:00.000Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":9,"cached_input_tokens":0,"output_tokens":1}}}}
{"timestamp":"2026-01-02T00:00:01.000Z","type":"turn_context","payload":{"model":"gpt-x"}}
{"timestamp":"2026-01-02T00:00:02.000Z","type":"response_item","payload":{"type":"custom_tool_call","call_id":"c1","name":"exec","input":"tools.exec_command({cmd:\"rg foo src\"})"}}
{"timestamp":"2026-01-02T00:00:03.000Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":400,"cached_input_tokens":300,"output_tokens":20}},"rate_limits":{"primary":{"used_percent":53.0,"window_minutes":10080}}}}
{"timestamp":"2026-01-02T00:00:04.000Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"c1","output":[{"type":"input_text","text":"abcdef"}]}}
{"timestamp":"2026-01-02T00:00:05.000Z","type":"response_item","payload":{"type":"custom_tool_call","call_id":"c2","name":"exec","input":"tools.apply_patch(\"...\")"}}
{"timestamp":"2026-01-02T00:00:06.000Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":600,"cached_input_tokens":500,"output_tokens":30}}}}"#;
        let r = codex(
            stdout,
            Some(rollout),
            (Some("2026-01-02T00:00:00.000Z"), None),
        );
        assert_eq!(
            r.summary.tokens,
            Tokens {
                input: 200,
                cache_read: 800,
                cache_write: 0,
                output: 50,
                reasoning: 20
            }
        );
        assert_eq!(r.per_call.len(), 2);
        assert_eq!(r.per_call[0].kind, "读取");
        assert_eq!(r.per_call[0].detail, "rg foo src");
        assert_eq!(r.per_call[0].output_bytes, 6);
        assert_eq!(r.per_call[1].kind, "编辑");
        assert_eq!(r.summary.models, vec!["gpt-x".to_owned()]);
        assert_eq!(r.summary.quota, Some(json!({"seven_day": 0.53})));
        // Stopped mid-turn: the calls are the total.
        let r = codex(
            r#"{"type":"thread.started","thread_id":"t"}"#,
            Some(rollout),
            (Some("2026-01-02T00:00:00.000Z"), None),
        );
        assert_eq!(r.summary.tokens.total(), 1050);
        let a = analysis(&r);
        assert_eq!(a["by_kind"][0]["kind"], "编辑");
    }
}
