use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output},
};

pub const MODEL_A: &str = "team/qa@high";
pub const MODEL_B: &str = "team/other";

pub fn catalog() -> Value {
    json!([
        {"ref":{"providerId":"team","modelId":"qa"},"providerLabel":"Team","label":"QA",
         "reasoning":{"levels":[{"value":"low","label":"Low"},{"value":"high","label":"High"}],"defaultLevel":"high"}},
        {"ref":{"providerId":"team","modelId":"other"},"providerLabel":"Team","label":"Other",
         "reasoning":{"levels":[],"defaultLevel":null}},
        {"ref":{"providerId":"other","modelId":"qa"},"providerLabel":"Other","label":"QA",
         "reasoning":{"levels":[{"value":"low","label":"Low"}],"defaultLevel":"low"}}
    ])
}

pub struct Fixture {
    _temp: tempfile::TempDir,
    pub home: PathBuf,
    cwd: PathBuf,
    user_home: PathBuf,
    bin: PathBuf,
    calls: PathBuf,
}

impl Fixture {
    pub fn new() -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let home = temp.path().join("library");
        let cwd = temp.path().join("workspace");
        let user_home = temp.path().join("user-home");
        let bin = temp.path().join("bin");
        let calls = temp.path().join("zcode-calls");
        for dir in [&cwd, &user_home, &bin] {
            fs::create_dir(dir)?;
        }
        let response = json!({"id":1,"result":{"settings":{"model":{"available":catalog()}}}});
        let binary = bin.join("zcode");
        // Only catalog RPCs are allowed: an accidental prompt fails the fixture.
        fs::write(
            &binary,
            format!(
                r#"#!/bin/sh
set -eu
test "$#" = 1
test "$1" = app-server
IFS= read -r request
case "$request" in *'"method":"session/create"'*) ;; *) exit 8 ;; esac
printf '%s\n' "$request" >> "$ZCODE_TWO_ROUNDS_CALLS"
printf '%s\n' '{response}'
IFS= read -r close
case "$close" in *'"method":"session/close"'*) ;; *) exit 9 ;; esac
"#
            ),
        )?;
        fs::set_permissions(binary, fs::Permissions::from_mode(0o700))?;
        Ok(Self {
            _temp: temp,
            home,
            cwd,
            user_home,
            bin,
            calls,
        })
    }

    pub fn invoke(&self, args: &[&str]) -> Result<Output> {
        Ok(self
            .command()
            .args(["project", "--home"])
            .arg(&self.home)
            .args(args)
            .output()?)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aep"));
        command
            .current_dir(&self.cwd)
            .env_clear()
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env_remove("AEP_PROJECT")
            .env_remove("AEP_WORK")
            .env_remove("AEP_PROJECT_HOME")
            .env("HOME", &self.user_home)
            .env("PATH", &self.bin)
            .env("TMPDIR", self._temp.path())
            .env("ZCODE_TWO_ROUNDS_CALLS", &self.calls)
            .env(
                "ZCODE_TWO_ROUNDS_RUN_CALLS",
                self._temp.path().join("run-calls"),
            );
        command
    }

    pub fn call(&self, args: &[&str]) -> Result<Value> {
        let output = self.invoke(args)?;
        ensure!(
            output.status.success(),
            "aep project {args:?}: {}\n{}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        Ok(serde_json::from_slice(&output.stdout)?)
    }

    pub fn catalog_calls(&self) -> Result<usize> {
        Ok(fs::read_to_string(&self.calls)?.lines().count())
    }

    pub fn repository() -> Result<Self> {
        let f = Self::new()?;
        // Only Git and our fake ZCode are reachable by the runner. Never fall
        // through to an installed model CLI or the user's Git configuration.
        std::os::unix::fs::symlink("/usr/bin/git", f.bin.join("git"))?;
        for args in [
            vec!["init", "-qb", "main"],
            vec!["config", "user.name", "AEP Test"],
            vec!["config", "user.email", "aep@example.invalid"],
            vec!["config", "commit.gpgsign", "false"],
        ] {
            f.git(&args)?;
        }
        fs::create_dir_all(f.cwd.join("specs/sample"))?;
        fs::write(
            f.cwd.join("specs/sample/spec.md"),
            "Write ok to result.txt.",
        )?;
        fs::write(
            f.cwd.join("specs/sample/task.json"),
            json!({"title":"fixture implementation","allowed_paths":["result.txt"],
                "check":["/bin/sh","-c","test \"$(/bin/cat result.txt)\" = ok"]})
            .to_string(),
        )?;
        fs::write(f.cwd.join("result.txt"), "old")?;
        f.git(&["add", "."])?;
        f.git(&["commit", "-qm", "fixture spec"])?;
        f.call(&["create", "two rounds", "--repository", "."])?;
        f.call(&["add-work", "model evidence"])?;
        f.call(&["zcode-default", "--model", MODEL_A])?;
        Ok(f)
    }

    fn git(&self, args: &[&str]) -> Result<()> {
        let command = self.command();
        // Reuse the isolated environment, but execute the known Git binary.
        let output = Command::new("/usr/bin/git")
            .current_dir(&self.cwd)
            .env_clear()
            .envs(command.get_envs().filter_map(|(k, v)| v.map(|v| (k, v))))
            .args(args)
            .output()?;
        ensure!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }

    pub fn data(&self) -> Result<PathBuf> {
        Ok(aep::workspace::Workspace::open(&self.cwd)?.data)
    }

    pub fn evidence(&self, id: &str) -> Result<Value> {
        let path = self.data()?.join("runs").join(id).join("evidence.json");
        let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
    }

    pub fn save_evidence(&self, id: &str, request: Option<Value>) -> Result<()> {
        let mut evidence = json!({"run_id":id,"status":"awaiting_human_review",
            "implementation_source":"zcode","reviewer":"zcode"});
        if let Some(request) = request {
            evidence["request"] = request;
        }
        aep::project::save_json(&self.run_dir(id)?, "evidence.json", &evidence)
    }

    pub fn run_dir(&self, id: &str) -> Result<PathBuf> {
        let dir = self.data()?.join("runs").join(id);
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    pub fn link_relay(&self, id: &str, attempts: &[(&str, &str)]) -> Result<()> {
        let dir = self.data()?.join("relays").join(id);
        fs::create_dir_all(&dir)?;
        aep::project::save_json(
            &dir,
            "relay.json",
            &json!({"id":id,
                "task":{"name":"sample"},
                "attempts":attempts.iter().map(|(id, round)| json!({"id":id,"round":round})).collect::<Vec<_>>()
            }),
        )?;
        self.call(&["link-record", "--repository", ".", "relay", id])?;
        Ok(())
    }

    pub fn enable_execution_stub(&self) -> Result<()> {
        // Catalog-only T1 fixtures still reject prompts. T2 explicitly opts in
        // to this local edit/review substitute; it cannot contact any service.
        fs::write(
            self.bin.join("zcode"),
            r#"#!/bin/sh
set -eu
if [ "$1" = app-server ]; then
    IFS= read -r create
    case "$create" in *'"method":"session/create"'*) ;; *) exit 8 ;; esac
    printf '{"kind":"create","message":%s}\n' "$create" >> "$ZCODE_TWO_ROUNDS_RUN_CALLS"
    sid=${create#*'"sessionId":"'}
    sid=${sid%%'"'*}
    case "$create" in *'"mode":"edit"'*) mode=edit ;; *'"mode":"plan"'*) mode=plan ;; *) exit 9 ;; esac
    case "$create" in
        *'"modelId":"other"'*) model=other ;;
        *'"modelId":"qa"'*) model=qa ;;
        *) exit 10 ;;
    esac
    printf '{"id":1,"result":{"settings":{"model":{"current":{"providerId":"team","modelId":"%s"}}}}}\n' "$model"
    IFS= read -r subscribe
    case "$subscribe" in *'"method":"session/subscribe"'*) ;; *) exit 11 ;; esac
    printf '%s\n' '{"id":2,"result":{}}'
    IFS= read -r prompt
    case "$prompt" in *'"method":"session/send"'*) ;; *) exit 12 ;; esac
    printf '{"kind":"send","message":%s}\n' "$prompt" >> "$ZCODE_TWO_ROUNDS_RUN_CALLS"
    printf '%s\n' '{"id":3,"result":{"accepted":true}}'
    printf '{"method":"session/event","params":{"sessionId":"%s","type":"turn.completed"}}\n' "$sid"
    IFS= read -r messages
    case "$messages" in *'"method":"session/messages"'*) ;; *) exit 13 ;; esac
else
    test "$#" = 7
    test "$1" = --mode
    mode=$2
    test "$3" = --no-color
    test "$4" = --prompt
    test "$6" = --attach
    test -f "$7"
    printf '{"kind":"native","mode":"%s"}\n' "$mode" >> "$ZCODE_TWO_ROUNDS_RUN_CALLS"
fi
if [ "$mode" = edit ]; then
    printf ok > result.txt
    reply='AEP-RELAY: {"note":"fixture implementation","question":""}'
else
    test "$mode" = plan
    reply='AEP-RELAY: {"note":"fixture review","question":"","report":"fixture passed","blocking_findings":[]}'
fi
if [ "$1" = app-server ]; then
    # Fixed JSON replies avoid a dependency on jq, Python or the real client.
    if [ "$mode" = edit ]; then
        printf '%s\n' '{"id":4,"result":{"messages":[{"info":{"role":"assistant"},"parts":[{"type":"text","text":"AEP-RELAY: {\"note\":\"fixture implementation\",\"question\":\"\"}"}]}]}}'
    else
        printf '%s\n' '{"id":4,"result":{"messages":[{"info":{"role":"assistant"},"parts":[{"type":"text","text":"AEP-RELAY: {\"note\":\"fixture review\",\"question\":\"\",\"report\":\"fixture passed\",\"blocking_findings\":[]}"}]}]}}'
    fi
else
    printf '%s\n' "$reply"
fi
"#,
        )?;
        Ok(())
    }

    pub fn run(&self, id: &str, model: Option<&str>, previous: Option<&str>) -> Result<Value> {
        let mut command = self.command();
        command.args([
            "run",
            "--task",
            "sample",
            "--base",
            "HEAD",
            "--implementer",
            "zcode",
            "--reviewer",
            "zcode",
            "--shared-memory",
            "--timeout",
            "10",
            "--run-id",
            id,
        ]);
        if let Some(model) = model {
            command.args(["--zcode-model", model]);
        }
        if let Some(previous) = previous {
            let evidence = self.evidence(previous)?;
            command.args([
                "--resume-from",
                evidence["candidate_sha"].as_str().unwrap(),
                "--handoff-from-run",
                previous,
            ]);
        }
        let output = command.output()?;
        let evidence = self.evidence(id).with_context(|| {
            format!(
                "aep run {id}: {}\n{}\n{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        })?;
        ensure!(
            output.status.success(),
            "aep run {id}: {}\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
            evidence
        );
        Ok(evidence)
    }

    pub fn run_calls(&self) -> Result<Vec<Value>> {
        fs::read_to_string(self._temp.path().join("run-calls"))?
            .lines()
            .map(|line| Ok(serde_json::from_str(line)?))
            .collect()
    }
}
