//! Minimal Claude Code launch for every CLI call AEP makes (stage runs, memory tidy,
//! independent review): no user MCP servers, user settings, slash commands/skills or
//! auto memory, so a call starts from the project's own context instead of ~280K of
//! user-level tools. Flags an older Claude Code does not know are left out (E3).
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Mutex, OnceLock, mpsc},
    time::{Duration, SystemTime},
};

/// Version of the "after" launch; evidence carrying `launch.diet` counts as optimised (R6).
pub const DIET: u32 = 1;

/// Flags the minimal launch adds, each with its value (if any).
pub const MINIMAL_FLAGS: [(&str, Option<&str>); 4] = [
    ("--strict-mcp-config", None),
    ("--setting-sources", Some("project,local")),
    ("--disable-slash-commands", None),
    ("--settings", Some(r#"{"autoMemoryEnabled":false}"#)),
];

const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeLaunch {
    /// Appended to the claude command line (includes `--model` only for the user default).
    pub args: Vec<String>,
    pub minimal: bool,
    /// Flags this Claude Code version does not support, so they were not passed (E3).
    pub not_minimized: Vec<String>,
    /// `Some("user_settings")` when the stage model was empty and the user's default was passed (E2).
    pub model_source: Option<&'static str>,
    /// Why `--help` could not be read; then every flag is assumed supported.
    pub probe_error: Option<String>,
}

impl ClaudeLaunch {
    /// The `launch` object of evidence.json, before init statistics are known.
    pub fn evidence(&self) -> Value {
        let mut v = json!({
            "diet": DIET,
            "minimal": self.minimal,
            "not_minimized": self.not_minimized,
            "model_source": self.model_source,
        });
        if let Some(e) = &self.probe_error {
            v["probe_error"] = json!(e);
        }
        v
    }
}

/// Minimal-launch arguments for `bin`. `model` is the profile model (may be empty).
pub fn claude_minimal(bin: &Path, model: &str) -> ClaudeLaunch {
    let user = if model.trim().is_empty() {
        user_model(&claude_config_dir())
    } else {
        None
    };
    let help = probe(bin);
    compose(help.as_deref().map_err(String::as_str), model, user)
}

/// Pure composition: `help` is the `--help` text or why it could not be read.
pub fn compose(help: Result<&str, &str>, model: &str, user_model: Option<String>) -> ClaudeLaunch {
    let mut args = Vec::new();
    let mut not_minimized = Vec::new();
    for (flag, value) in MINIMAL_FLAGS {
        if help.is_ok_and(|h| !supports(h, flag)) {
            not_minimized.push(flag.to_owned());
            continue;
        }
        args.push(flag.to_owned());
        if let Some(v) = value {
            args.push(v.to_owned());
        }
    }
    let mut model_source = None;
    if model.trim().is_empty()
        && let Some(m) = user_model.filter(|m| !m.trim().is_empty())
    {
        args.extend(["--model".to_owned(), m.trim().to_owned()]);
        model_source = Some("user_settings");
    }
    ClaudeLaunch {
        args,
        minimal: not_minimized.is_empty(),
        not_minimized,
        model_source,
        probe_error: help.err().map(str::to_owned),
    }
}

/// Whether `--help` output lists `flag` as an option.
pub fn supports(help: &str, flag: &str) -> bool {
    help.match_indices(flag).any(|(i, _)| {
        let before = help[..i].chars().next_back();
        let after = help[i + flag.len()..].chars().next();
        before.is_none_or(|c| c.is_whitespace() || matches!(c, ',' | '(' | '['))
            && after.is_none_or(|c| c.is_whitespace() || matches!(c, ',' | '=' | '<' | '['))
    })
}

/// `name` resolved on `$PATH` (or `name` itself when not found), for callers that run
/// a bare program name.
pub fn on_path(name: &str) -> PathBuf {
    std::env::var_os("PATH")
        .and_then(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join(name))
                .find(|c| c.is_file())
        })
        .unwrap_or_else(|| PathBuf::from(name))
}

/// Where Claude Code keeps user settings: `$CLAUDE_CONFIG_DIR`, else `~/.claude`.
pub fn claude_config_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|h| h.join(".claude")))
        .unwrap_or_default()
}

/// The `model` string of `<dir>/settings.json`, the only user setting kept (E2).
pub fn user_model(dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(dir.join("settings.json")).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    v["model"]
        .as_str()
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_owned)
}

type Probe = Result<String, String>;
type ProbeCache = Mutex<HashMap<(PathBuf, Option<SystemTime>), Probe>>;

/// `<bin> --help`, cached per executable path and modification time.
fn probe(bin: &Path) -> Probe {
    static CACHE: OnceLock<ProbeCache> = OnceLock::new();
    let key = (
        bin.to_path_buf(),
        bin.metadata().and_then(|m| m.modified()).ok(),
    );
    let cache = CACHE.get_or_init(Default::default);
    if let Some(hit) = cache.lock().unwrap().get(&key) {
        return hit.clone();
    }
    let result = run_help(bin);
    cache.lock().unwrap().insert(key, result.clone());
    result
}

fn run_help(bin: &Path) -> Probe {
    let mut child = Command::new(bin)
        .arg("--help")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("无法运行 {} --help：{e}", bin.display()))?;
    let mut out = child.stdout.take().expect("piped stdout");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let _ = out.read_to_string(&mut text);
        let _ = tx.send(text);
    });
    let text = match rx.recv_timeout(PROBE_TIMEOUT) {
        Ok(text) => text,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{} --help 超时", bin.display()));
        }
    };
    let _ = child.wait();
    // Test doubles and wrappers may print anything; only real help text is trusted.
    if !text.contains("Usage:") {
        return Err(format!("{} --help 输出无法识别", bin.display()));
    }
    Ok(text)
}

/// What a stream-json run actually loaded: counts and plugin sources from the `init`
/// event and the first API call's context from the first assistant event.
pub fn claude_init_stats(stdout: &str) -> Option<Value> {
    let mut init = None;
    let mut first_context = None;
    for line in stdout.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if init.is_none() && v["type"] == "system" && v["subtype"] == "init" {
            init = Some(v);
        } else if first_context.is_none() && v["type"] == "assistant" {
            let u = &v["message"]["usage"];
            if u.is_object() {
                first_context = Some(
                    [
                        "input_tokens",
                        "cache_creation_input_tokens",
                        "cache_read_input_tokens",
                    ]
                    .iter()
                    .filter_map(|k| u[k].as_u64())
                    .sum::<u64>(),
                );
            }
        }
        if init.is_some() && first_context.is_some() {
            break;
        }
    }
    let init = init?;
    let list = |k: &str| init[k].as_array().cloned().unwrap_or_default();
    let tools = list("tools");
    let plugins = list("plugins");
    let builtin: Vec<String> = plugins
        .iter()
        .filter(|p| {
            p["source"]
                .as_str()
                .is_some_and(|s| s.ends_with("@builtin"))
        })
        .filter_map(|p| p["name"].as_str().map(str::to_owned))
        .collect();
    let plugin_sources: Vec<_> = plugins
        .iter()
        .map(|plugin| {
            let name = plugin["name"]
                .as_str()
                .or_else(|| plugin.as_str())
                .filter(|s| !s.is_empty());
            let origin = ["source", "path"]
                .into_iter()
                .filter_map(|key| plugin[key].as_str())
                .find(|s| !s.is_empty());
            // Claude can report the complete name@source, or only the origin.
            if let (Some(name), Some(origin)) = (name, origin)
                && !origin
                    .strip_prefix(name)
                    .is_some_and(|suffix| suffix.starts_with('@'))
            {
                return format!("{name}@{}", origin.strip_prefix('@').unwrap_or(origin));
            }
            origin.or(name).unwrap_or("未知插件").to_owned()
        })
        .collect();
    Some(json!({
        "mcp_servers": list("mcp_servers").len(),
        "mcp_tools": tools.iter().filter(|t| t.as_str().is_some_and(|t| t.starts_with("mcp__"))).count(),
        "tools": tools.len(),
        "skills": list("skills").len(),
        "plugins": plugins.len(),
        "builtin_plugins": builtin,
        "plugin_sources": plugin_sources,
        "first_context": first_context,
        "version": init["claude_code_version"],
    }))
}

/// Merges init statistics into a `launch` evidence object.
pub fn with_stats(mut launch: Value, stats: Option<Value>) -> Value {
    if let (Some(o), Some(Value::Object(s))) = (launch.as_object_mut(), stats) {
        o.extend(s);
    }
    launch
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELP: &str = "Usage: claude [options] [command] [prompt]\n\
  --disable-slash-commands              Disable all skills and commands\n\
  --setting-sources <sources>           Comma-separated list\n\
  --settings <file-or-json>             Settings file or JSON string\n\
  --strict-mcp-config                   Only use MCP servers from --mcp-config\n";

    #[test]
    fn launch_passes_every_minimal_flag_when_supported() {
        let l = compose(Ok(HELP), "sonnet", Some("opus".into()));
        assert_eq!(
            l.args,
            [
                "--strict-mcp-config",
                "--setting-sources",
                "project,local",
                "--disable-slash-commands",
                "--settings",
                r#"{"autoMemoryEnabled":false}"#
            ]
        );
        assert!(l.minimal && l.not_minimized.is_empty() && l.model_source.is_none());
        assert!(!l.args.iter().any(|a| a == "--mcp-config" || a == "--bare"));
        let e = l.evidence();
        assert_eq!(e["diet"], DIET);
        assert_eq!(e["minimal"], true);
    }

    #[test]
    fn launch_leaves_out_flags_an_old_version_lacks() {
        let old =
            "Usage: claude [options]\n  --settings <file-or-json>  x\n  --mcp-config <c>  y\n";
        let l = compose(Ok(old), "m", None);
        assert_eq!(l.args, ["--settings", r#"{"autoMemoryEnabled":false}"#]);
        assert!(!l.minimal);
        assert_eq!(
            l.not_minimized,
            [
                "--strict-mcp-config",
                "--setting-sources",
                "--disable-slash-commands"
            ]
        );
        assert_eq!(
            l.evidence()["not_minimized"],
            json!([
                "--strict-mcp-config",
                "--setting-sources",
                "--disable-slash-commands"
            ])
        );
    }

    #[test]
    fn launch_assumes_support_when_help_cannot_be_read() {
        let l = compose(Err("x --help 超时"), "m", None);
        assert!(l.minimal);
        assert_eq!(l.args.len(), 6);
        assert_eq!(l.evidence()["probe_error"], "x --help 超时");
    }

    #[test]
    fn launch_falls_back_to_the_user_default_model_only_when_empty() {
        let l = compose(Ok(HELP), "  ", Some("opus".into()));
        assert_eq!(&l.args[l.args.len() - 2..], ["--model", "opus"]);
        assert_eq!(l.evidence()["model_source"], "user_settings");
        let l = compose(Ok(HELP), "", None);
        assert!(!l.args.contains(&"--model".to_owned()));
        assert!(l.evidence()["model_source"].is_null());
    }

    #[test]
    fn launch_reads_only_the_model_from_user_settings() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(user_model(t.path()), None);
        std::fs::write(
            t.path().join("settings.json"),
            r#"{"model":" opus ","permissions":{"allow":["mcp__x"]},"hooks":{}}"#,
        )
        .unwrap();
        assert_eq!(user_model(t.path()).as_deref(), Some("opus"));
        std::fs::write(t.path().join("settings.json"), "not json").unwrap();
        assert_eq!(user_model(t.path()), None);
    }

    #[test]
    fn launch_flag_detection_needs_a_whole_option() {
        assert!(supports(HELP, "--settings"));
        assert!(!supports("  --settings-file <f>\n", "--settings"));
        assert!(supports("only (--settings, --agents)", "--settings"));
        assert!(!supports(
            "  --strict-mcp-config-x\n",
            "--strict-mcp-config"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn launch_probe_runs_help_once_and_rejects_non_help_output() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let log = t.path().join("calls");
        let bin = t.path().join("claude");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\nprintf 'Usage: claude\\n  --settings <x>\\n'\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
        let a = claude_minimal(&bin, "m");
        let b = claude_minimal(&bin, "m");
        assert_eq!(a, b);
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "--help\n");
        assert_eq!(a.not_minimized.len(), 3);

        let double = t.path().join("double");
        std::fs::write(
            &double,
            "#!/bin/sh\ncat >/dev/null\necho '{\"type\":\"result\"}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&double, std::fs::Permissions::from_mode(0o700)).unwrap();
        let l = claude_minimal(&double, "m");
        assert!(l.minimal && l.probe_error.is_some());
    }

    #[test]
    fn launch_init_stats_count_what_was_loaded() {
        let out = [
            r#"{"type":"system","subtype":"hook_started","session_id":"s"}"#,
            r#"{"type":"system","subtype":"init","claude_code_version":"2.1.331","tools":["Read","Grep","Glob","Bash","mcp__x__a"],"mcp_servers":[{"name":"x"}],"skills":[],"plugins":[{"name":"agents-md","source":"agents-md@builtin"},{"name":"lsp","source":"lsp@official"}]}"#,
            r#"{"type":"assistant","message":{"usage":{"input_tokens":3,"cache_creation_input_tokens":9000,"cache_read_input_tokens":797}}}"#,
            r#"{"type":"assistant","message":{"usage":{"input_tokens":1,"cache_read_input_tokens":99999}}}"#,
        ]
        .join("\n");
        let s = claude_init_stats(&out).unwrap();
        assert_eq!(
            s,
            json!({"mcp_servers":1,"mcp_tools":1,"tools":5,"skills":0,"plugins":2,
                   "builtin_plugins":["agents-md"],"plugin_sources":["agents-md@builtin","lsp@official"],
                   "first_context":9800,"version":"2.1.331"})
        );
        assert!(claude_init_stats(r#"{"type":"result","result":"x"}"#).is_none());
        let merged = with_stats(compose(Ok(HELP), "m", None).evidence(), Some(s));
        assert_eq!(merged["diet"], DIET);
        assert_eq!(merged["first_context"], 9800);
        assert_eq!(
            merged["plugin_sources"],
            json!(["agents-md@builtin", "lsp@official"])
        );
    }

    #[test]
    fn launch_plugin_sources_prefer_source_then_path_then_name() {
        let init = json!({"type": "system", "subtype": "init", "plugins": [
            {"name": "market", "source": "official", "path": "/ignored"},
            {"name": "local", "path": "/plugins/Local Tools"},
            {"name": "agents-md", "path": "@builtin"},
            {"name": "standalone"},
            "legacy",
            {},
        ]});
        let stats = claude_init_stats(&init.to_string()).unwrap();
        assert_eq!(
            stats["plugin_sources"],
            json!([
                "market@official",
                "local@/plugins/Local Tools",
                "agents-md@builtin",
                "standalone",
                "legacy",
                "未知插件",
            ])
        );
        assert_eq!(stats["plugins"], 6);
        assert_eq!(stats["builtin_plugins"], json!([]));
    }
}
