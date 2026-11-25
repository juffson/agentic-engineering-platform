//! Tidy a project's memory with a model CLI: it reads every current note and proposes merges,
//! rewrites and retirements. Nothing changes until a person accepts operations; accepted ones
//! go through the version history (revise / retire), so every change can be undone.
use crate::{
    memory::{Kind, Memory, TEXT_LIMIT},
    workflow::{Client, Profile},
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Several notes say the same thing: one new note replaces them all.
    Merge {
        ids: Vec<i64>,
        text: String,
        why: String,
    },
    /// One note reworded (clearer, shorter, corrected).
    Rewrite { id: i64, text: String, why: String },
    /// A note that no longer helps (stale, finished, contradicted): out of what agents read.
    Retire { id: i64, why: String },
}

impl Op {
    pub fn ids(&self) -> Vec<i64> {
        match self {
            Op::Merge { ids, .. } => ids.clone(),
            Op::Rewrite { id, .. } | Op::Retire { id, .. } => vec![*id],
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub summary: String,
    pub ops: Vec<Op>,
}

/// The notes agents currently read: newest version of each, not retired.
pub fn current(dir: &Path) -> Result<Vec<Value>> {
    Ok(Memory::browse(dir)?
        .into_iter()
        .filter(|n| n["successor"].is_null() && n["scope"] == "project")
        .collect())
}

pub fn prompt(notes: &[Value], instruction: &str) -> String {
    let mut text = String::from(
        "你在整理一个软件项目的「项目记忆」。每个开发阶段的 Agent 开始工作前都会读这些笔记，但每次最多只读最新的 32 条，所以笔记要少而精。\n\n\
         请通读下面全部笔记，提出整理方案：\n\
         - merge：几条说的是同一件事、或可以归成一条的，合并成一条新笔记（写全要点，不丢信息）。\n\
         - rewrite：一条笔记含糊、啰嗦、有过时细节，改写得清楚简短。\n\
         - retire：已经过时、已完成且不再有指导意义、或被其他笔记取代的，移出（会留在历史里）。\n\
         没有问题的笔记不要动。不要编造笔记里没有的事实；拿不准是否过时的就保留。每条新文本不超过 900 字，用笔记原来的语言。\n\n",
    );
    if !instruction.trim().is_empty() {
        text.push_str(&format!("整理要求：{}\n\n", instruction.trim()));
    }
    text.push_str("## 当前笔记\n");
    for n in notes {
        text.push_str(&format!(
            "\n#{} [{}]\n{}\n",
            n["id"],
            n["kind"].as_str().unwrap_or(""),
            n["body"].as_str().unwrap_or("")
        ));
    }
    text.push_str(
        "\n## 输出\n只输出一个 JSON 对象，不要别的文字：\n\
         {\"summary\":\"一两句话说明整理思路\",\"ops\":[\n\
         {\"op\":\"merge\",\"ids\":[3,7],\"text\":\"合并后的笔记\",\"why\":\"原因\"},\n\
         {\"op\":\"rewrite\",\"id\":5,\"text\":\"改写后的笔记\",\"why\":\"原因\"},\n\
         {\"op\":\"retire\",\"id\":9,\"why\":\"原因\"}]}\n\
         每个笔记编号最多出现在一个操作里。",
    );
    text
}

/// Read the proposal out of the model's answer and check it only touches current notes.
pub fn parse(answer: &str, notes: &[Value]) -> Result<Proposal> {
    let start = answer.find('{').context("答复里没有 JSON")?;
    let end = answer.rfind('}').context("答复里没有 JSON")?;
    let proposal: Proposal =
        serde_json::from_str(&answer[start..=end]).context("无法解析整理方案 JSON")?;
    let known: HashSet<i64> = notes.iter().filter_map(|n| n["id"].as_i64()).collect();
    let mut used = HashSet::new();
    let mut ops = Vec::new();
    for op in proposal.ops {
        let ids = op.ids();
        // Drop operations on unknown or already-claimed notes rather than fail the whole run.
        if ids.is_empty() || !ids.iter().all(|i| known.contains(i) && !used.contains(i)) {
            continue;
        }
        match &op {
            Op::Merge { ids, text, .. } if ids.len() < 2 || text.trim().is_empty() => continue,
            Op::Merge { text, .. } | Op::Rewrite { text, .. }
                if text.trim().chars().count() > TEXT_LIMIT =>
            {
                continue;
            }
            Op::Rewrite { text, .. } if text.trim().is_empty() => continue,
            _ => {}
        }
        used.extend(ids);
        ops.push(op);
    }
    Ok(Proposal {
        summary: proposal.summary,
        ops,
    })
}

fn kind(note: &Value) -> Kind {
    match note["kind"].as_str() {
        Some("handoff") => Kind::Handoff,
        Some("open_question") => Kind::OpenQuestion,
        Some("review") => Kind::Review,
        _ => Kind::Decision,
    }
}

/// Apply accepted operations. Every note they touch must still be current.
pub fn apply(dir: &Path, ops: &[Op]) -> Result<usize> {
    let notes = current(dir)?;
    let find = |id: i64| notes.iter().find(|n| n["id"] == id);
    for op in ops {
        for id in op.ids() {
            ensure!(
                find(id).is_some(),
                "记忆 #{id} 在提出方案后已被修改，请重新整理"
            );
        }
    }
    let mut db = Memory::open(dir)?;
    for op in ops {
        match op {
            Op::Rewrite { id, text, .. } => {
                db.revise(*id, kind(find(*id).unwrap()), text.trim())?;
            }
            Op::Retire { id, why } => {
                db.retire(*id, &format!("整理时移出：{why}"))?;
            }
            Op::Merge { ids, text, .. } => {
                let new = db.revise(ids[0], kind(find(ids[0]).unwrap()), text.trim())?;
                for id in &ids[1..] {
                    db.retire(*id, &format!("整理时合并进 #{new}"))?;
                }
            }
        }
    }
    Ok(ops.len())
}

/// A tidy job: `job.json` (profile, instruction) in, `status.json` and `proposal.json` out.
pub fn run(memory_dir: &Path, job: &Path) -> Result<()> {
    let spec = crate::workspace::read(&job.join("job.json"))?;
    let status = |s: &str, extra: Value| -> Result<()> {
        let mut v = json!({"status": s, "updated_at": chrono::Utc::now().to_rfc3339()});
        if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
            o.extend(e.clone());
        }
        crate::project::save_json(job, "status.json", &v)
    };
    status("running", json!({}))?;
    let result = (|| -> Result<Proposal> {
        let profile: Profile = serde_json::from_value(spec["profile"].clone())?;
        let notes = current(memory_dir)?;
        ensure!(!notes.is_empty(), "还没有项目记忆");
        let text = prompt(&notes, spec["instruction"].as_str().unwrap_or(""));
        fs::write(job.join("prompt.txt"), &text)?;
        let answer = ask(&profile, &text, job, job)?;
        fs::write(job.join("result.txt"), &answer)?;
        parse(&answer, &notes)
    })();
    match result {
        Ok(p) => {
            crate::project::save_json(job, "proposal.json", &p)?;
            status("succeeded", json!({"ops": p.ops.len()}))
        }
        Err(e) => status("failed", json!({"error": format!("{e:#}")})),
    }
}

/// One non-interactive, read-only turn of the CLI, working in `cwd`; logs go to `dir`.
pub fn ask(profile: &Profile, prompt: &str, dir: &Path, cwd: &Path) -> Result<String> {
    let bin = profile.program(&std::env::var_os("PATH").unwrap_or_default())?;
    let mut cmd = Command::new(&bin);
    let answer = dir.join("answer.txt");
    match profile.client {
        Client::Codex => {
            cmd.args(["exec", "--sandbox", "read-only", "--skip-git-repo-check"])
                .arg("--output-last-message")
                .arg(&answer);
        }
        Client::Claude => {
            cmd.args(["-p", "--output-format", "json", "--tools", "Read,Grep,Glob"]);
            cmd.args(crate::launch::claude_minimal(&bin, &profile.model).args);
        }
        _ => bail!("目前支持 Codex 和 Claude"),
    }
    if !profile.model.trim().is_empty() {
        cmd.args(["--model", profile.model.trim()]);
    }
    if profile.client == Client::Codex {
        cmd.arg("-");
    }
    let mut child = cmd
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(fs::File::create(dir.join("stderr.log"))?)
        .spawn()
        .with_context(|| format!("无法启动 {}", bin.display()))?;
    child
        .stdin
        .take()
        .context("stdin")?
        .write_all(prompt.as_bytes())?;
    let out = child.wait_with_output()?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    fs::write(dir.join("stdout.log"), &stdout)?;
    if !out.status.success() {
        let reason = crate::transcript::failure(&stdout)
            .unwrap_or_else(|| crate::workflow::tail(&dir.join("stderr.log"), 800));
        bail!(
            "{} 退出码 {:?}：{reason}",
            profile.client.label(),
            out.status.code()
        );
    }
    match profile.client {
        Client::Codex => Ok(fs::read_to_string(answer).context("Codex 未生成答复")?),
        _ => {
            let v: Value = serde_json::from_str(stdout.trim()).context("Claude 输出无法解析")?;
            ensure!(v["is_error"] != true, "Claude：{}", v["result"]);
            Ok(v["result"]
                .as_str()
                .context("Claude 未生成答复")?
                .to_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn launch_ask_runs_claude_with_the_minimal_flags() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let log = t.path().join("argv");
        let bin = t.path().join("claude");
        fs::write(
            &bin,
            format!(
                r#"#!/bin/sh
case "$1" in --help)
  printf 'Usage: claude\n  --strict-mcp-config\n  --setting-sources <s>\n  --disable-slash-commands\n  --settings <f>\n'
  exit 0 ;;
esac
cat >/dev/null
printf '%s\n' "$@" > '{}'
printf '%s\n' '{{"type":"result","result":"好","is_error":false}}'
"#,
                log.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();
        let profile = Profile {
            client: Client::Claude,
            executable: bin.display().to_string(),
            model: "sonnet".into(),
            check_command: String::new(),
        };
        assert_eq!(ask(&profile, "整理", t.path(), t.path()).unwrap(), "好");
        let argv = fs::read_to_string(&log).unwrap();
        let argv: Vec<&str> = argv.lines().collect();
        for flag in [
            "--strict-mcp-config",
            "--disable-slash-commands",
            "project,local",
            r#"{"autoMemoryEnabled":false}"#,
        ] {
            assert!(argv.contains(&flag), "{flag} missing in {argv:?}");
        }
        assert_eq!(argv.iter().filter(|a| **a == "--model").count(), 1);
        assert!(!argv.contains(&"--mcp-config"));
    }

    #[test]
    fn keeps_only_valid_operations_on_current_notes() {
        let notes = vec![json!({"id":1}), json!({"id":2}), json!({"id":3})];
        let answer = r#"好的：
        {"summary":"合并重复","ops":[
          {"op":"merge","ids":[1,2],"text":"合并","why":"重复"},
          {"op":"retire","id":2,"why":"已在上面使用"},
          {"op":"rewrite","id":9,"text":"x","why":"不存在"},
          {"op":"merge","ids":[3],"text":"只有一条","why":"无效"},
          {"op":"retire","id":3,"why":"过时"}]}"#;
        let p = parse(answer, &notes).unwrap();
        assert_eq!(p.summary, "合并重复");
        assert_eq!(p.ops.len(), 2);
        assert!(matches!(p.ops[0], Op::Merge { .. }));
        assert_eq!(
            p.ops[1],
            Op::Retire {
                id: 3,
                why: "过时".into()
            }
        );
    }

    #[test]
    fn applying_merges_rewrites_and_retires_through_history() -> Result<()> {
        let t = tempfile::tempdir()?;
        let dir = t.path();
        let mut db = Memory::open(dir)?;
        let k = crate::memory::Key::new("", "", "");
        let a = db.add_manual(&k, "decision", "金额用分", None, None)?;
        let b = db.add_manual(&k, "decision", "金额统一用分存储", None, None)?;
        let c = db.add_manual(&k, "decision", "旧部署流程", None, None)?;
        let d = db.add_manual(&k, "decision", "构建要JDK17啦", None, None)?;
        drop(db);
        let ops = vec![
            Op::Merge {
                ids: vec![a, b],
                text: "金额统一用整数分存储".into(),
                why: "重复".into(),
            },
            Op::Retire {
                id: c,
                why: "已废弃".into(),
            },
            Op::Rewrite {
                id: d,
                text: "Android 构建需要 JDK 17".into(),
                why: "更清楚".into(),
            },
        ];
        assert_eq!(apply(dir, &ops)?, 3);
        let now: Vec<String> = current(dir)?
            .iter()
            .map(|n| n["body"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(now.len(), 2, "{now:?}");
        assert!(now.contains(&"金额统一用整数分存储".to_owned()));
        assert!(now.contains(&"Android 构建需要 JDK 17".to_owned()));
        // Agents read neither the merged-away nor the retired notes.
        let ctx = Memory::open(dir)?.workflow_context(None)?;
        assert_eq!(ctx.len(), 2);
        // A stale proposal is refused.
        assert!(apply(dir, &ops).is_err());
        Ok(())
    }
}
