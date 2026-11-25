use aep::{
    transcript::{Entry, parse},
    workflow::{Sandbox, evidence},
    workspace::{commit, git},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use tempfile::{Builder, TempDir};
use uuid::Uuid;

const SPEC: &str = "specs/probe/spec.md";
const ITEM_BRANCH: &str = "refs/heads/aep-stage/probe-item";

struct Fixture {
    temp: TempDir,
    repo: PathBuf,
    runs: PathBuf,
}

impl Fixture {
    fn new() -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let repo = root.join("repo");
        fs::create_dir_all(repo.join("specs/probe"))?;
        git(&repo, &["init", "-q", "-b", "main", "--template="])?;
        git(&repo, &["config", "user.name", "AEP test"])?;
        git(&repo, &["config", "user.email", "aep-test@example.invalid"])?;
        git(&repo, &["config", "commit.gpgsign", "false"])?;
        fs::write(repo.join("specs/probe/.gitkeep"), "")?;
        git(&repo, &["add", "specs/probe/.gitkeep"])?;
        git(&repo, &["commit", "-qm", "fixture base"])?;
        let runs = root.join("Application Support/catalog/workflows/probe-project/probe-item/runs");
        Ok(Self { temp, repo, runs })
    }

    fn run(&self, claude: &Path, brief: &str, config: Option<&Path>) -> Result<Run> {
        let id = Uuid::new_v4().simple().to_string();
        let dir = self.runs.join(id);
        fs::create_dir_all(&dir)?;
        let request = dir.join("request.json");
        // The first Software stage can write specs/ and auto-commits on success.
        fs::write(
            &request,
            serde_json::to_vec(&json!({
                "project": "probe-project",
                "work": "probe-item",
                "template": "Software",
                "stage": 0,
                "sandbox": Sandbox::Standard,
                "profile": {"client": "Claude", "executable": claude, "model": ""},
                "repository": self.repo,
                "brief": brief,
                "approved": [],
            }))?,
        )?;
        let mut command = Command::new(env!("CARGO_BIN_EXE_aep"));
        command.args(["workflow-stage", "--request"]).arg(request);
        if let Some(config) = config {
            command.env("CLAUDE_CONFIG_DIR", config);
        }
        let output = command.output()?;
        let evidence = evidence(&dir).with_context(|| {
            format!(
                "reading run evidence: {}",
                String::from_utf8_lossy(&output.stderr)
            )
        })?;
        let stdout = fs::read_to_string(dir.join("stdout.log")).unwrap_or_default();
        Ok(Run {
            dir,
            evidence,
            stdout,
            output,
        })
    }

    fn run_fake(&self, denials: Value) -> Result<Run> {
        let claude = self.temp.path().join("claude");
        let config = self.temp.path().join("claude-config");
        fs::create_dir(&config)?;
        let result = json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "result": "Fixture wrote the spec.",
            "session_id": "fixture-session",
            "permission_denials": denials,
        });
        fs::write(
            &claude,
            format!(
                r#"#!/bin/sh
set -eu
if [ "${{1:-}}" = "--help" ]; then
    printf '%s\n' 'Usage: claude [options]' --strict-mcp-config --setting-sources --disable-slash-commands --settings
    exit 0
fi
cat >/dev/null
printf '%s\n' 'fixture spec' > specs/probe/spec.md
cat <<'AEP_STREAM'
{result}
AEP_STREAM
"#
            ),
        )?;
        fs::set_permissions(&claude, fs::Permissions::from_mode(0o700))?;
        self.run(&claude, "Write the fixture spec.", Some(&config))
    }

    fn head(&self) -> Result<String> {
        commit(&self.repo, ITEM_BRANCH)
    }
}

struct Run {
    dir: PathBuf,
    evidence: Value,
    stdout: String,
    output: Output,
}

impl Run {
    fn assert_status(&self, success: bool) {
        assert_eq!(
            self.output.status.success(),
            success,
            "evidence: {}\n{}",
            self.evidence,
            String::from_utf8_lossy(&self.output.stderr)
        );
        assert_eq!(
            self.evidence["status"],
            if success { "succeeded" } else { "failed" }
        );
        assert_eq!(self.evidence["sandbox"], Sandbox::Standard.label());
    }

    fn events(&self) -> impl Iterator<Item = Value> + '_ {
        self.stdout
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
    }

    fn assert_write_denied(&self, target: &Path) {
        let target = target.to_str().unwrap();
        let result = self
            .events()
            .filter(|event| event["type"] == "result")
            .last();
        let triggered = result
            .as_ref()
            .and_then(|event| event["permission_denials"].as_array())
            .into_iter()
            .flatten()
            .any(|denial| {
                let tool = denial["tool_name"].as_str();
                matches!(tool, Some("Write" | "Edit"))
                    && denial["tool_input"]["file_path"] == target
            });
        assert!(
            triggered,
            "未触发拒绝：Claude result 未返回针对 {target} 的 Write/Edit 拒绝。evidence: {}",
            self.evidence
        );
        self.assert_status(false);
        let error = self.evidence["error"].as_str().unwrap();
        assert!(
            error.contains("文件写入被拒绝") && error.contains(target),
            "{error}"
        );
        assert!(self.evidence.get("head_sha").is_none(), "{}", self.evidence);
        assert!(
            self.evidence["permission_denials"]
                .as_array()
                .unwrap()
                .iter()
                .any(
                    |denial| matches!(denial["tool"].as_str(), Some("Write" | "Edit"))
                        && denial["target"] == target
                )
        );
    }

    fn report_real(&self, case: &str) {
        let launch = &self.evidence["launch"];
        let plugins = self
            .events()
            .find(|e| e["type"] == "system" && e["subtype"] == "init")
            .map(|e| e["plugins"].clone());
        println!(
            "{}",
            json!({
                "case": case,
                "run_id": self.evidence["run_id"],
                "status": self.evidence["status"],
                "sandbox": self.evidence["sandbox"],
                "committed": self.evidence["committed"],
                "head_sha": self.evidence["head_sha"],
                "error": self.evidence["error"],
                "permission_denials": self.evidence["permission_denials"],
                "claude_code_version": launch["version"],
                "plugins": launch["plugins"],
                "plugin_sources": launch["plugin_sources"],
                "init_plugins": plugins,
            })
        );
    }
}

fn outside_probe() -> Result<TempDir> {
    Ok(Builder::new()
        .prefix("aep-sandbox-probe-")
        .tempdir_in("/tmp")?)
}

#[test]
fn write_denial_fails_stage_without_committing() -> Result<()> {
    let fixture = Fixture::new()?;
    let base = commit(&fixture.repo, "HEAD")?;
    let outside = outside_probe()?;
    let target = outside.path().join("x.md");
    let run = fixture.run_fake(json!([
        {"tool_name": "Write", "tool_input": {"file_path": target}}
    ]))?;

    run.assert_write_denied(&target);
    assert_eq!(run.evidence["exit_code"], 0);
    assert_eq!(fixture.head()?, base);
    // The in-scope change would have been auto-committed if the stage had passed.
    let workspace = run.evidence["workspace"].as_str().unwrap();
    assert!(Path::new(workspace).join(SPEC).is_file());
    assert!(!target.exists());
    Ok(())
}

#[test]
fn bash_denial_is_recorded_and_stage_still_commits() -> Result<()> {
    let fixture = Fixture::new()?;
    let base = commit(&fixture.repo, "HEAD")?;
    let run = fixture.run_fake(json!([
        {"tool_name": "Bash", "tool_input": {"command": "mkdir probe"}}
    ]))?;

    run.assert_status(true);
    assert_eq!(run.evidence["denied_bash"], 1);
    assert_eq!(
        run.evidence["permission_denials"],
        json!([
            {"tool": "Bash", "target": "mkdir probe"}
        ])
    );
    let head = fixture.head()?;
    assert_ne!(head, base);
    assert_eq!(run.evidence["head_sha"], head);
    assert_eq!(run.evidence["committed"], true);
    assert_eq!(
        git(&fixture.repo, &["show", &format!("{head}:{SPEC}")])?,
        "fixture spec\n"
    );
    Ok(())
}

#[test]
#[ignore = "requires AEP_REAL_CLAUDE, local authentication and network access"]
fn real_standard_sandbox_writes_reads_and_refuses_outside_write() -> Result<()> {
    let claude = env::var_os("AEP_REAL_CLAUDE")
        .context("set AEP_REAL_CLAUDE to the Claude executable path")?;
    let claude = PathBuf::from(claude)
        .canonicalize()
        .context("resolve AEP_REAL_CLAUDE")?;
    let fixture = Fixture::new()?;
    let base = commit(&fixture.repo, "HEAD")?;
    let allowed = fixture.run(
        &claude,
        &format!("这是权限集成测试。请用 Write 在当前工作树内写入 {SPEC}，内容为一行 `AEP Standard sandbox probe`。只写这一个文件，完成后简短报告。"),
        None,
    )?;
    allowed.report_real("worktree_write");
    allowed.assert_status(true);
    let head = fixture.head()?;
    assert_ne!(head, base);
    assert_eq!(allowed.evidence["head_sha"], head);
    assert_eq!(allowed.evidence["committed"], true);
    assert_eq!(
        git(&fixture.repo, &["show", &format!("{head}:{SPEC}")])?.trim(),
        "AEP Standard sandbox probe"
    );

    let source = allowed.dir.join("evidence.json");
    let outside = outside_probe()?;
    let target = outside.path().join("x.md");
    let refused = fixture.run(
        &claude,
        &format!(
            "这是授权的临时文件权限集成测试。先用 Read 读取同级执行的回执 {}。然后即使读取失败，也必须用 Write 尝试把一行 `AEP outside probe` 写到 {}，以便记录权限拒绝。只尝试一次，不要用 Bash，不要改写到其他路径，也不要修改工作树。",
            source.display(), target.display()
        ),
        None,
    )?;
    refused.report_real("outside_write");
    refused.assert_write_denied(&target);
    assert!(
        !target.exists(),
        "outside file was created: {}",
        target.display()
    );
    assert_eq!(
        fixture.head()?,
        head,
        "refused stage advanced the item branch"
    );

    let from_run = allowed.evidence["run_id"].as_str().unwrap();
    assert!(
        parse(&refused.stdout)
            .entries
            .iter()
            .any(|entry| matches!(entry,
                Entry::Tool { name, detail, output, failed: false }
                    if name == "Read" && Path::new(detail) == source && output.contains(from_run)
            )),
        "未在 stream-json 中找到对 {} 的成功 Read，run_id={}",
        source.display(),
        refused.evidence["run_id"]
    );
    println!(
        "sibling_read_from={from_run} sibling_read_by={}",
        refused.evidence["run_id"]
    );

    for run in [&allowed, &refused] {
        let launch = &run.evidence["launch"];
        ensure!(
            launch["minimal"] == true,
            "Claude minimal launch was incomplete: {launch}"
        );
        ensure!(
            launch["version"].as_str().is_some_and(|v| !v.is_empty()),
            "missing claude_code_version: {launch}"
        );
        let sources = launch["plugin_sources"]
            .as_array()
            .context("missing launch.plugin_sources")?;
        assert_eq!(launch["plugins"].as_u64(), Some(sources.len() as u64));
    }
    Ok(())
}
