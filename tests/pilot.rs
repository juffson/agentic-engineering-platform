use serde_json::{Value, json};
use std::{
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

fn ok(out: Output) -> String {
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}
fn git(repo: &Path, args: &[&str]) -> String {
    ok(Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap())
}
struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    root: PathBuf,
    base: String,
    bin: PathBuf,
}
impl Fixture {
    fn new(check: &[&str]) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_owned();
        let repo = root.join("repo");
        let bin = root.join("bin");
        fs::create_dir(&bin).unwrap();
        fs::create_dir(&repo).unwrap();
        git(&root, &["init", "--bare", "remote"]);
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.name", "AEP"]);
        git(&repo, &["config", "user.email", "test@example.invalid"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        git(
            &repo,
            &[
                "remote",
                "add",
                "origin",
                root.join("remote").to_str().unwrap(),
            ],
        );
        fs::create_dir_all(repo.join("specs/sample")).unwrap();
        fs::write(repo.join("specs/sample/spec.md"), "Write ok to result.txt.").unwrap();
        fs::write(
            repo.join("specs/sample/task.json"),
            json!({"title":"implement","allowed_paths":["result.txt"],"check":check}).to_string(),
        )
        .unwrap();
        fs::write(repo.join("result.txt"), "old").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "base"]);
        let base = git(&repo, &["rev-parse", "HEAD"]);
        let f = Self {
            _tmp: tmp,
            root,
            repo,
            base,
            bin,
        };
        ok(f.call(&["dispatch", "--task", "sample"]));
        f
    }
    fn cmd(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut dirs = vec![self.bin.clone()];
        dirs.extend(env::split_paths(&env::var_os("PATH").unwrap()));
        let mut c = Command::new(env!("CARGO_BIN_EXE_aep"));
        c.current_dir(cwd)
            .args(args)
            .env("PATH", env::join_paths(dirs).unwrap())
            .env("TMPDIR", &self.root)
            .env("FIXTURE", &self.root);
        c
    }
    fn call(&self, args: &[&str]) -> Output {
        self.cmd(&self.repo, args).output().unwrap()
    }
    fn state(&self, id: &str) -> Value {
        serde_json::from_str(&ok(self.call(&["status", "--run-id", id]))).unwrap()
    }
    fn script(&self, name: &str, body: &str) {
        let p = self.bin.join(name);
        fs::write(&p, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
        fs::set_permissions(p, fs::Permissions::from_mode(0o755)).unwrap();
    }
}
#[test]
fn pinned_dispatch_replay_and_scope() {
    let f = Fixture::new(&["grep", "-qx", "ok", "result.txt"]);
    ok(f.call(&["dispatch", "--task", "sample"]));
    let run = [
        "run",
        "--task",
        "sample",
        "--candidate",
        "HEAD",
        "--run-id",
        "demo",
    ];
    assert!(!f.call(&run).status.success());
    let red = f.state("demo");
    assert_eq!(red["check_exit_code"], 1);
    assert!(!f.call(&run).status.success());
    assert_eq!(red, f.state("demo"));
    fs::write(f.repo.join("result.txt"), "ok").unwrap();
    git(&f.repo, &["commit", "-am", "fix"]);
    assert!(!f.call(&["dispatch", "--task", "sample"]).status.success());
    assert!(!f.call(&run).status.success());
    assert_eq!(red, f.state("demo"));
    let green = [
        "run",
        "--task",
        "sample",
        "--candidate",
        "HEAD",
        "--run-id",
        "success",
    ];
    ok(f.call(&green));
    let report = f.state("success");
    assert_eq!(report["spec_sha"], f.base);
    assert_eq!(
        report["candidate_sha"],
        git(&f.repo, &["rev-parse", "HEAD"])
    );
    assert_eq!(report["approved"], false);
    ok(f.call(&green));
    assert_eq!(report, f.state("success"));
    fs::write(f.repo.join("specs/sample/spec.md"), "changed").unwrap();
    git(&f.repo, &["commit", "-am", "edit spec"]);
    assert!(
        !f.call(&[
            "verify",
            "--task",
            "sample",
            "--base",
            &f.base,
            "--candidate",
            "HEAD",
            "--run-id",
            "outside"
        ])
        .status
        .success()
    );
    assert!(
        f.state("outside")["error"]
            .as_str()
            .unwrap()
            .contains("outside the task")
    );
}
#[test]
fn busy_and_interrupted_attempts_do_not_restart() {
    for crash in [false, true] {
        let f = Fixture::new(&[
            "sh",
            "-c",
            r#"echo started >> "$FIXTURE/starts"; while [ ! -f "$FIXTURE/release" ]; do sleep 0.05; done"#,
        ]);
        let args = [
            "run",
            "--task",
            "sample",
            "--candidate",
            "HEAD",
            "--run-id",
            "locked",
            "--timeout",
            "20",
        ];
        let mut child = f.cmd(&f.repo, &args).stdout(Stdio::null()).spawn().unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        while !f.root.join("starts").exists() {
            assert!(Instant::now() < until);
            thread::sleep(Duration::from_millis(10));
        }
        let running = f.state("locked");
        assert_eq!(running["status"], "running");
        assert_eq!(running["phase"], "checking");
        assert_eq!(f.call(&args).status.code(), Some(75));
        if crash {
            child.kill().unwrap();
        }
        fs::write(f.root.join("release"), "").unwrap();
        let exit = child.wait().unwrap();
        if crash {
            let r = f.state("locked");
            assert_eq!(r["status"], "needs_reconciliation");
            assert!(Path::new(r["worktree"].as_str().unwrap()).exists());
            assert!(!f.call(&args).status.success());
        } else {
            assert!(exit.success());
            ok(f.call(&args));
        }
        assert_eq!(
            fs::read_to_string(f.root.join("starts")).unwrap(),
            "started\n"
        );
    }
}
#[test]
fn timeout_stops_the_check_process_group() {
    let f = Fixture::new(&["sh", "-c", r#"(sleep 2; touch "$FIXTURE/escaped") & wait"#]);
    let check = f.call(&[
        "verify",
        "--task",
        "sample",
        "--base",
        "HEAD",
        "--candidate",
        "HEAD",
        "--run-id",
        "timeout",
        "--timeout",
        "1",
    ]);
    assert!(!check.status.success());
    let report = f.state("timeout");
    assert_eq!(report["status"], "failed");
    assert!(report["error"].as_str().unwrap().contains("timed out"));
    thread::sleep(Duration::from_millis(1200));
    assert!(!f.root.join("escaped").exists());
}

#[test]
fn cancellation_stops_only_the_selected_run_and_keeps_its_evidence() {
    let f = Fixture::new(&["true"]);
    f.script(
        "codex",
        r#"echo 'partial implementation'
echo active > "$FIXTURE/started"
(sleep 2; touch "$FIXTURE/escaped") & wait"#,
    );
    let args = [
        "run",
        "--task",
        "sample",
        "--implementer",
        "codex",
        "--run-id",
        "stopped",
        "--timeout",
        "10",
    ];
    let child = f.cmd(&f.repo, &args).stdout(Stdio::null()).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !f.root.join("started").exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    let data = f.repo.join(".git/aep");
    let cancellation = aep::cancel::Cancellation::new(&data, "stopped").unwrap();
    assert!(cancellation.request().unwrap());
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    let record = f.state("stopped");
    assert_eq!(record["status"], "cancelled");
    assert_eq!(record["phase"], "implementing");
    assert!(record["finished_at"].is_string());
    assert!(Path::new(record["recovery_worktree"].as_str().unwrap()).is_dir());
    let logs = data.join("runs/stopped");
    assert!(
        fs::read_to_string(logs.join("implementation.log"))
            .unwrap()
            .contains("partial implementation")
    );
    assert!(!logs.join("checks.log").exists());
    assert!(!cancellation.request().unwrap());

    ok(f.call(&[
        "run",
        "--task",
        "sample",
        "--candidate",
        "HEAD",
        "--run-id",
        "survivor",
    ]));
    assert_eq!(f.state("survivor")["status"], "awaiting_human_review");
    thread::sleep(Duration::from_secs(2));
    assert!(!f.root.join("escaped").exists());
}

#[test]
fn old_evidence_is_read_only_and_ids_are_validated() {
    let f = Fixture::new(&["true"]);
    let dir = f.repo.join(".git/aep/runs/legacy");
    fs::create_dir_all(&dir).unwrap();
    let old = json!({"run_id":"legacy","status":"awaiting_human_review","approved":false});
    fs::write(dir.join("evidence.json"), old.to_string()).unwrap();
    assert_eq!(f.state("legacy"), old);
    assert!(
        !f.call(&[
            "run",
            "--task",
            "sample",
            "--candidate",
            "HEAD",
            "--run-id",
            "legacy"
        ])
        .status
        .success()
    );
    assert!(
        !f.call(&["status", "--run-id", "../unsafe"])
            .status
            .success()
    );
}
#[test]
fn two_adapters_pass_scoped_notes_across_worktrees() {
    let f = Fixture::new(&["grep", "-qx", "ok", "result.txt"]);
    let linked = f.root.join("review-worker");
    git(
        &f.repo,
        &[
            "worktree",
            "add",
            "--detach",
            linked.to_str().unwrap(),
            &f.base,
        ],
    );
    ok(f.cmd(
        &linked,
        &[
            "memory",
            "add",
            "--project",
            "--kind",
            "decision",
            "--text",
            "stable contract",
        ],
    )
    .output()
    .unwrap());
    f.script(
        "codex",
        r#"prompt=$(cat)
case "$prompt" in *"stable contract"*) ;; *) exit 1;; esac
while [ "$1" != --output-last-message ]; do shift; done
printf '%s' '{"note":"handoff-719","question":"Which deadline?"}' > "$2"
printf ok > result.txt
echo log-only-canary"#,
    );
    let long_review_note = "阅".repeat(aep::memory::TEXT_LIMIT + 1);
    let review = json!({"structured_output":{
        "note":long_review_note,"question":"","report":"detailed report"
    }});
    f.script(
        "claude",
        &format!(
            r#"prompt=$(cat)
case "$prompt" in *"handoff-719"*) ;; *) exit 1;; esac
case "$prompt" in *log-only-canary*) exit 1;; esac
echo diagnostics >&2
printf '%s' '{review}'"#
        ),
    );
    let args = [
        "run",
        "--task",
        "sample",
        "--implementer",
        "codex",
        "--reviewer",
        "claude",
        "--run-id",
        "both",
        "--shared-memory",
        "--codex-model",
        "demo-model",
    ];
    ok(f.call(&args));
    let r = f.state("both");
    let sha = r["candidate_sha"].as_str().unwrap();
    let handoff = &r["memory_outputs"]["implementer"][0];
    assert_eq!(r["review_status"], "findings_recorded");
    let reviewer_note = r["memory_outputs"]["reviewer"][0].as_i64().unwrap();
    let data = f.repo.join(".git/aep");
    let db = aep::memory::Memory::open(&data).unwrap();
    assert_eq!(
        db.get(reviewer_note).unwrap()["body"],
        format!("{}…", "阅".repeat(aep::memory::TEXT_LIMIT - 1))
    );
    let review_folder = data.join("runs/both");
    assert_eq!(
        fs::read_to_string(review_folder.join("review.txt")).unwrap(),
        "detailed report\n"
    );
    assert_eq!(
        aep::workspace::read(&review_folder.join("review-result.json")).unwrap(),
        review
    );
    assert_eq!(r["request"]["codex_model"], "demo-model");
    assert!(
        r["memory_inputs"]["reviewer"]
            .as_array()
            .unwrap()
            .contains(handoff)
    );
    ok(f.call(&args));
    assert_eq!(r, f.state("both"));
    let check = [
        "verify",
        "--task",
        "sample",
        "--base",
        &f.base,
        "--candidate",
        sha,
        "--shared-memory",
        "--reviewer",
        "claude",
        "--run-id",
        "separate",
    ];
    ok(f.cmd(&linked, &check).output().unwrap());
    assert!(
        f.state("separate")["memory_inputs"]["reviewer"]
            .as_array()
            .unwrap()
            .contains(handoff)
    );
    let workspace = aep::workspace::Workspace::open(&linked).unwrap();
    let mut db = aep::memory::Memory::open(&workspace.data).unwrap();
    let revised = db
        .revise(
            handoff.as_i64().unwrap(),
            aep::memory::Kind::Handoff,
            "updated-handoff",
        )
        .unwrap();
    f.script(
        "claude",
        r#"prompt=$(cat)
case "$prompt" in *handoff-719*) exit 1;; esac
case "$prompt" in *updated-handoff*) ;; *) exit 1;; esac
echo '{"structured_output":{"note":"seen revision","question":"","report":"review"}}'"#,
    );
    let mut check_revised = check;
    *check_revised.last_mut().unwrap() = "after-revision";
    ok(f.cmd(&linked, &check_revised).output().unwrap());
    assert!(
        f.state("after-revision")["memory_inputs"]["reviewer"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!(revised))
    );
    assert_eq!(f.state("both"), r);
    let ctx = ok(f.call(&["memory", "list", "--task", "sample", "--spec", &f.base]));
    assert!(ctx.contains("Which deadline?"));
    assert!(!ctx.contains("handoff-719"));
    assert!(!ctx.contains("log-only-canary"));
}
#[test]
fn failed_checks_and_invalid_handoffs_stay_out_of_shared_context() {
    let f = Fixture::new(&["grep", "-qx", "ok", "result.txt"]);
    f.script(
        "claude",
        r#"prompt=$(cat)
case "$prompt" in *"verified-handoff"*) ;; *) exit 1;; esac
case "$prompt" in *"failed-run-question"*|*"invalid-handoff"*) exit 1;; esac
echo '{"structured_output":{"note":"verified-review","question":"","report":"review"}}'"#,
    );
    for (id, source, handoff, check_exit) in [
        (
            "failed-check",
            "wrong",
            r#"{"note":"failed-handoff","question":"failed-run-question"}"#,
            1,
        ),
        (
            "invalid-handoff",
            "ok",
            r#"{"note":"invalid-handoff","question":42}"#,
            0,
        ),
        (
            "next-run",
            "ok",
            r#"{"note":"verified-handoff","question":""}"#,
            0,
        ),
    ] {
        f.script(
            "codex",
            &format!(
                r#"prompt=$(cat)
case "$prompt" in *"failed-run-question"*|*"invalid-handoff"*) exit 1;; esac
while [ "$1" != --output-last-message ]; do shift; done
printf '%s' '{handoff}' > "$2"
printf '%s\n' '{source}' > result.txt"#
            ),
        );
        let output = f.call(&[
            "run",
            "--task",
            "sample",
            "--implementer",
            "codex",
            "--reviewer",
            "claude",
            "--shared-memory",
            "--run-id",
            id,
        ]);
        let record = f.state(id);
        assert_eq!(record["check_exit_code"], check_exit);
        if id == "next-run" {
            ok(output);
            let handoff = &record["memory_outputs"]["implementer"][0];
            assert!(handoff.is_number());
            assert_eq!(record["memory_inputs"]["reviewer"], json!([handoff]));
            assert_eq!(record["review_status"], "findings_recorded");
        } else {
            assert!(!output.status.success());
            assert_eq!(record["status"], "failed");
            assert_eq!(record["memory_outputs"], json!({}));
            assert!(record["memory_inputs"]["reviewer"].is_null());
            let ws = aep::workspace::Workspace::open(&f.repo).unwrap();
            assert!(aep::studio::Snapshot::load(&ws).notes.is_empty());
        }
    }
}

#[test]
fn plain_adapters_do_not_open_a_memory_database() {
    let f = Fixture::new(&["grep", "-qx", "ok", "result.txt"]);
    f.script("codex", "cat >/dev/null; printf ok >result.txt");
    f.script("claude", "cat >/dev/null; echo findings");
    ok(f.call(&[
        "run",
        "--task",
        "sample",
        "--implementer",
        "codex",
        "--reviewer",
        "claude",
        "--run-id",
        "plain",
    ]));
    let r = f.state("plain");
    assert_eq!(r["review_status"], "findings_recorded");
    assert_eq!(r["approved"], false);
    assert!(!f.repo.join(".git/aep/memory.sqlite3").exists());
}

#[test]
fn manual_memory_resolves_refs_and_rejects_a_candidate_behind_the_spec() {
    use aep::memory::Key;
    let f = Fixture::new(&["true"]);
    git(&f.repo, &["commit", "--allow-empty", "-m", "candidate"]);
    let tip = git(&f.repo, &["rev-parse", "HEAD"]);
    let key = Key::resolve(&f.repo, Some("sample"), Some("HEAD~1"), Some("HEAD")).unwrap();
    assert_eq!(key.spec, f.base);
    assert_eq!(key.candidate, tip);
    assert!(Key::resolve(&f.repo, Some("sample"), Some("HEAD"), Some("HEAD~1")).is_err());
    assert!(Key::resolve(&f.repo, None, None, Some("HEAD")).is_err());
    assert!(Key::resolve(&f.repo, Some("sample"), None, None).is_err());
}

#[test]
fn acceptance_plan_records_criteria_failures_and_checks_not_run() {
    let f = Fixture::new(&["true"]);
    let manifest = f.repo.join("specs/sample/task.json");
    fs::write(&manifest, json!({
        "title": "Implement",
        "allowed_paths": ["result.txt"],
        "check": ["true"],
        "criteria": ["AC-1"],
        "checks": [
            {"id":"integration","criteria":["AC-2"],"command":["sh","-c","echo failing-extra; exit 5"]},
            {"id":"later","criteria":["AC-3"],"command":["true"]}
        ]
    }).to_string()).unwrap();
    git(
        &f.repo,
        &["commit", "-am", "Pin multiple acceptance checks"],
    );
    let result = f.call(&[
        "verify",
        "--task",
        "sample",
        "--base",
        "HEAD",
        "--candidate",
        "HEAD",
        "--run-id",
        "criteria",
    ]);
    assert!(!result.status.success());
    let state = f.state("criteria");
    assert_eq!(state["check_exit_code"], 5);
    assert_eq!(state["acceptance"][0]["criteria"], json!(["AC-1"]));
    assert_eq!(state["acceptance"][0]["status"], "passed");
    assert_eq!(state["acceptance"][1]["exit_code"], 5);
    assert_eq!(state["acceptance"][2]["status"], "not_run");
    let log = fs::read_to_string(f.repo.join(".git/aep/runs/criteria/checks.log")).unwrap();
    assert!(log.contains("failing-extra"));
}

#[test]
fn explicit_review_blocker_keeps_a_passing_candidate_out_of_delivery() {
    let f = Fixture::new(&["true"]);
    let manifest = f.repo.join("specs/sample/task.json");
    fs::write(
        &manifest,
        json!({
            "allowed_paths":["result.txt"], "check":["true"], "criteria":["AC-1"]
        })
        .to_string(),
    )
    .unwrap();
    git(&f.repo, &["commit", "-am", "Pin criterion"]);
    f.script("claude", r#"printf '%s' '{"structured_output":{"note":"","question":"","report":"Missing requirement", "blocking_findings":[{"criterion":"AC-1","file":"result.txt","line":1,"reason":"missing result"}]}}'"#);
    let result = f.call(&[
        "verify",
        "--task",
        "sample",
        "--base",
        "HEAD",
        "--candidate",
        "HEAD",
        "--run-id",
        "blocked",
        "--reviewer",
        "claude",
    ]);
    assert!(!result.status.success());
    let state = f.state("blocked");
    assert_eq!(state["check_exit_code"], 0);
    assert_eq!(state["status"], "failed");
    assert_eq!(state["review_status"], "changes_requested");
    assert_eq!(state["blocking_findings"][0]["criterion"], "AC-1");
}

#[test]
fn control_worker_recovers_after_restart_and_delivery_recovers_a_lost_receipt() {
    use std::io::{BufRead, BufReader};

    struct Server(std::process::Child);
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let f = Fixture::new(&[
        "sh",
        "-c",
        r#"touch "$FIXTURE/checking"; while [ ! -f "$FIXTURE/release" ]; do sleep 0.1; done; grep -qx ok result.txt"#,
    ]);
    let origin = "https://github.com/example/pilot.git";
    git(&f.repo, &["push", "origin", "main"]);
    git(&f.repo, &["remote", "set-url", "origin", origin]);
    let local = f.root.join("remote");
    git(
        &f.repo,
        &[
            "config",
            &format!("url.{}.insteadOf", local.display()),
            origin,
        ],
    );
    f.script(
        "git",
        &format!(
            r#"case "$*" in
"remote get-url origin"|"remote get-url --push --all origin") echo "{origin}" ;;
*) exec /usr/bin/git "$@" ;;
esac"#
        ),
    );
    f.script(
        "codex",
        r#"
if [ "$1" = --version ]; then echo codex-test; exit; fi
test -z "${AEP_CONTROL_OPERATOR_TOKEN:-}${AEP_CONTROL_DEVICE_TOKEN:-}"
echo started >> "$FIXTURE/starts"
echo sensitive-local-log-canary
while [ "$1" != --output-last-message ]; do shift; done
printf '%s' '{"note":"changed result","question":""}' > "$2"
printf ok > result.txt
"#,
    );
    f.script(
        "claude",
        r#"
if [ "$1" = --version ]; then echo claude-test; exit; fi
test -z "${AEP_CONTROL_OPERATOR_TOKEN:-}${AEP_CONTROL_DEVICE_TOKEN:-}"
cat >/dev/null
printf '%s' '{"structured_output":{"note":"reviewed","question":"","report":"No blockers"}}'
"#,
    );
    f.script("gh", r#"
case "$*" in
*" api user"*|*"github.com user"*) echo '{"login":"operator"}' ;;
*"--method POST"*)
    test ! -f "$FIXTURE/pull.json"
    git rev-parse refs/heads/main > "$FIXTURE/base"
    head=$(git ls-remote --heads origin 'aep/pr/*' | cut -f1)
    base=$(cat "$FIXTURE/base")
    printf '{"number":123,"state":"open","head":{"sha":"%s"},"base":{"ref":"main","sha":"%s"}}\n' "$head" "$base" > "$FIXTURE/pull.json"
    exit 1 ;;
*"--method GET"*)
    if [ -f "$FIXTURE/pull.json" ]; then printf '['; cat "$FIXTURE/pull.json"; printf ']'; else echo '[]'; fi ;;
*) exit 2 ;;
esac
"#);

    let operator = "operator-token-test-000000000000000000000";
    let device = "device-token-test-000000000000000000000";
    let configured = |args: &[&str]| {
        let mut command = f.cmd(&f.repo, args);
        command
            .env("AEP_CONTROL_OPERATOR_TOKEN", operator)
            .env("AEP_CONTROL_DEVICE_TOKEN", device);
        command
    };
    let store = f.root.join("control.json");
    let mut controller = Server(
        configured(&[
            "control",
            "serve",
            "--listen",
            "127.0.0.1:0",
            "--state",
            store.to_str().unwrap(),
            "--device",
            "test-device",
            "--lease-seconds",
            "40",
            "--require-evidence-grant",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap(),
    );
    let mut line = String::new();
    BufReader::new(controller.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let address = line.split_whitespace().last().unwrap();
    let endpoint = format!("http://{address}");
    let call = |action: &[&str]| {
        let mut args = vec!["control", "--url", endpoint.as_str()];
        args.extend(action);
        serde_json::from_str::<Value>(&ok(configured(&args).output().unwrap())).unwrap()
    };
    assert!(
        !configured(&["control", "--url", &endpoint, "status"])
            .env("AEP_CONTROL_OPERATOR_TOKEN", device)
            .output()
            .unwrap()
            .status
            .success()
    );
    let submit = [
        "submit",
        "--ticket",
        "test",
        "--task",
        "sample",
        "--spec",
        &f.base,
        "--target",
        "main",
        "--baseline",
        &f.base,
        "--timeout",
        "30",
    ];
    let job = call(&submit);
    assert_eq!(call(&submit), job);
    let run = job["data"]["id"].as_str().unwrap();
    let worker = ["control", "--url", &endpoint, "worker", "--once"];
    let mut first = configured(&worker).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !f.root.join("checking").is_file() {
        assert!(
            Instant::now() < deadline,
            "worker never reached independent checks"
        );
        thread::sleep(Duration::from_millis(15));
    }
    first.kill().unwrap();
    first.wait().unwrap();
    let restarted = configured(&worker)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    fs::write(f.root.join("release"), "").unwrap();
    ok(restarted.wait_with_output().unwrap());
    assert_eq!(
        fs::read_to_string(f.root.join("starts")).unwrap(),
        "started\n"
    );
    let snapshot = call(&["status"]);
    let attempt = &snapshot["data"]["attempts"][0];
    assert_eq!(attempt["stage"], "awaiting_approval");
    assert_eq!(attempt["profile"]["codex"], "codex-test");
    assert!(attempt.get("fence").is_none());
    assert_eq!(
        attempt["events"].as_array().unwrap().last().unwrap()["progress"]["outcome"],
        "passed"
    );
    assert!(
        !fs::read_to_string(&store)
            .unwrap()
            .contains("sensitive-local-log-canary")
    );
    let private: Value = serde_json::from_slice(&fs::read(&store).unwrap()).unwrap();
    let fence = private["attempts"][0]["fence"].as_str().unwrap();
    let url = format!("{endpoint}/v1/evidence/{run}");
    let http = reqwest::blocking::Client::new();
    let permission = |proof: &str| {
        http.head(&url)
            .bearer_auth(device)
            .header("x-aep-fence", proof)
            .send()
            .unwrap()
            .status()
    };
    let mut packet = b"AEP-EVIDENCE-1\n".to_vec();
    let canary = b"retention-test-packet";
    packet.extend((canary.len() as u32).to_be_bytes());
    packet.extend(canary);
    for _ in 0..10 {
        packet.extend(u32::MAX.to_be_bytes());
    }
    let upload = || {
        http.put(&url)
            .bearer_auth(device)
            .header("x-aep-fence", fence)
            .body(packet.clone())
            .send()
            .unwrap()
            .status()
    };
    assert_eq!(permission(fence), reqwest::StatusCode::FORBIDDEN);
    assert_eq!(upload(), reqwest::StatusCode::FORBIDDEN);
    assert!(!store.with_extension("evidence").join(run).exists());
    assert_eq!(
        http.get(&url)
            .bearer_auth(operator)
            .header("x-aep-ticket", "test")
            .send()
            .unwrap()
            .status(),
        reqwest::StatusCode::FORBIDDEN
    );
    call(&[
        "evidence-grant",
        "--ticket",
        "test",
        "--run",
        run,
        "--seconds",
        "120",
    ]);
    assert_eq!(permission(fence), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(permission("wrong-fence"), reqwest::StatusCode::CONFLICT);
    assert_eq!(upload(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(upload(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(
        http.get(&url)
            .bearer_auth(operator)
            .header("x-aep-ticket", "test")
            .send()
            .unwrap()
            .bytes()
            .unwrap()
            .as_ref(),
        packet.as_slice()
    );

    let candidate = f.state(run)["candidate_sha"].as_str().unwrap().to_owned();
    let confirmed = [
        "--ticket",
        "test",
        "--run",
        run,
        "--spec",
        &f.base,
        "--candidate",
        &candidate,
    ];
    let args = || {
        let mut result = vec!["publish"];
        result.extend(confirmed);
        result
    };
    let mut before_approval = vec!["control", "--url", &endpoint];
    before_approval.extend(args());
    assert!(
        !configured(&before_approval)
            .output()
            .unwrap()
            .status
            .success()
    );
    let mut approving = vec!["approve"];
    approving.extend(confirmed);
    let approval = call(&approving);
    assert_eq!(approval["data"]["stage"], "approved");
    assert_eq!(
        approval["data"]["approval"]["report_sha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    let log = f.repo.join(".git/aep/runs").join(run).join("checks.log");
    let original = fs::read(&log).unwrap();
    fs::write(&log, "changed after approval").unwrap();
    assert!(
        !configured(&before_approval)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(!f.root.join("pull.json").exists());
    fs::write(&log, original).unwrap();
    let publication = call(&args());
    assert_eq!(publication["data"]["pull_request"], 123);
    assert_eq!(call(&args()), publication);
    assert_eq!(
        git(&f.repo, &["ls-remote", "--heads", "origin", "aep/pr/*"])
            .lines()
            .count(),
        1
    );
}

fn local_task(f: &Fixture, operation: &[&str]) -> Value {
    let args = ["local"]
        .into_iter()
        .chain(operation.iter().copied())
        .collect::<Vec<_>>();
    serde_json::from_str(&ok(f.call(&args))).unwrap()
}

#[test]
fn local_plan_review_feedback_approval_and_merge_are_recoverable() {
    let f = Fixture::new(&["sh", "-ec", "grep -Eq '^(first|corrected)$' result.txt"]);
    git(&f.repo, &["switch", "-qc", "spec"]);
    let path = f.repo.join("specs/sample/task.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    manifest["criteria"] = json!(["AC-1"]);
    fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    git(&f.repo, &["commit", "-qam", "numbered acceptance"]);
    let spec = git(&f.repo, &["rev-parse", "HEAD"]);
    git(&f.repo, &["switch", "-q", "main"]);
    git(&f.repo, &["remote", "remove", "origin"]);
    f.script("gh", "exit 88");
    f.script(
        "codex",
        r#"
prompt=$(cat)
while [ "$1" != --output-last-message ]; do shift; done
printf '{"note":"implemented","question":""}\n' > "$2"
case "$prompt" in
    *needs-correction*) test "$(cat result.txt)" = first; printf 'corrected\n' > result.txt ;;
    *) printf 'first\n' > result.txt ;;
esac
printf '%s\n' '{"type":"turn.completed","usage":{"input_tokens":12,"output_tokens":8}}'
"#,
    );
    f.script("claude", r#"
cat >/dev/null
case "$(cat result.txt)" in
first)
    printf '%s\n' '{"structured_output":{"note":"blocked","question":"","report":"Needs correction","blocking_findings":[{"criterion":"AC-1","file":"result.txt","line":1,"reason":"needs-correction"}]},"usage":{"input_tokens":4,"output_tokens":6},"total_cost_usd":0.001}'
    ;;
corrected)
    printf '%s\n' '{"structured_output":{"note":"corrected","question":"","report":"Passed","blocking_findings":[]},"usage":{"input_tokens":4,"output_tokens":6},"total_cost_usd":0.001}'
    ;;
esac
"#);
    let proposed = local_task(
        &f,
        &[
            "plan", "--ticket", "issue", "--task", "sample", "--spec", &spec,
        ],
    );
    assert_eq!(proposed["phase"], "plan_pending");
    assert!(
        !f.call(&["local", "run", "--ticket", "issue"])
            .status
            .success()
    );
    let plan = [
        "--ticket",
        "issue",
        "--version",
        "1",
        "--spec",
        &spec,
        "--baseline",
        &f.base,
    ];
    let approve = ["approve-plan"].into_iter().chain(plan).collect::<Vec<_>>();
    assert_eq!(local_task(&f, &approve)["phase"], "ready");
    assert!(
        !f.call(&["local", "run", "--ticket", "issue"])
            .status
            .success()
    );
    let red = local_task(&f, &["status", "--ticket", "issue"]);
    assert_eq!(red["phase"], "retry");
    assert_eq!(red["run"]["review_status"], "changes_requested");
    assert_eq!(
        red["run"]["blocking_findings"][0]["reason"],
        "needs-correction"
    );
    ok(f.call(&["local", "run", "--ticket", "issue"]));

    let green = local_task(&f, &["inspect", "--ticket", "issue"]);
    assert_eq!(green["phase"], "review_pending");
    assert_eq!(
        green["run"]["request"]["feedback_run"],
        red["run"]["run_id"]
    );
    assert_eq!(green["run"]["request"]["whole_attempt"], true);
    assert!(green["review"]["text"].as_str().unwrap().contains("Passed"));
    assert!(
        green["diff"]["text"]
            .as_str()
            .unwrap()
            .contains("+corrected")
    );
    assert_eq!(green["usage"]["tokens"], 60);
    assert_eq!(green["usage"]["unpriced_calls"], 2);
    let previous = red["run"]["run_id"].as_str().unwrap();
    let history = local_task(&f, &["inspect", "--ticket", "issue", "--run", previous]);
    assert_eq!(history["phase"], "history");
    assert_eq!(history["historical"], true);
    assert_eq!(history["run"]["review_status"], "changes_requested");
    assert!(
        history["review"]["text"]
            .as_str()
            .unwrap()
            .contains("Needs correction")
    );
    assert!(history["report_sha256"].is_null());
    assert!(
        !f.call(&[
            "local",
            "inspect",
            "--ticket",
            "issue",
            "--run",
            "unrelated"
        ])
        .status
        .success()
    );
    let run = green["run"]["run_id"].as_str().unwrap();
    let candidate = green["run"]["candidate_sha"].as_str().unwrap();
    let scope = plan
        .into_iter()
        .chain(["--run", run, "--candidate", candidate])
        .collect::<Vec<_>>();
    let mut merge = vec!["local", "merge"];
    merge.extend(scope.iter().copied());
    assert!(!f.call(&merge).status.success());

    let digest = green["report_sha256"].as_str().unwrap();
    let approve = ["approve-delivery"]
        .into_iter()
        .chain(scope)
        .chain([
            "--report-sha256",
            digest,
            "--human-minutes",
            "5",
            "--baseline-minutes",
            "12",
        ])
        .collect::<Vec<_>>();
    assert_eq!(local_task(&f, &approve)["phase"], "approved");
    let review_path = f.repo.join(".git/aep/runs").join(run).join("review.txt");
    let original = fs::read(&review_path).unwrap();
    fs::write(&review_path, "changed after approval").unwrap();
    assert!(!f.call(&merge).status.success());
    fs::write(&review_path, original).unwrap();
    fs::write(f.repo.join("dirty"), "protect local files").unwrap();
    assert!(!f.call(&merge).status.success());
    fs::remove_file(f.repo.join("dirty")).unwrap();

    ok(f.call(&merge));
    let state_path = f.repo.join(".git/aep/local-tasks/issue.json");
    let mut persisted: Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    persisted["current"]["merged_at"] = Value::Null;
    fs::write(state_path, serde_json::to_vec(&persisted).unwrap()).unwrap();
    ok(f.call(&merge));
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), candidate);
    assert_eq!(
        fs::read_to_string(f.repo.join("result.txt")).unwrap(),
        "corrected\n"
    );
    let report = local_task(&f, &["report"]);
    assert_eq!(report["tasks"][0]["phase"], "merged");
    assert_eq!(report["tasks"][0]["versions"][0]["human_minutes"], 5);
    assert_eq!(report["tasks"][0]["versions"][0]["baseline_minutes"], 12);
}

#[test]
fn local_authoring_freezes_the_target_and_retains_previous_specs() {
    let f = Fixture::new(&["true"]);
    git(&f.repo, &["remote", "remove", "origin"]);
    f.script("gh", "exit 88");
    assert_eq!(
        local_task(&f, &["targets"]),
        json!([{
            "name": "main", "sha": f.base, "checked_out": true,
        }])
    );
    let draft = f.root.join("draft.json");
    let mut content = json!({
        "name": "authored", "spec": "# Original\n",
        "manifest": {"title": "Studio task", "allowed_paths": ["result.txt"], "check": ["true"]},
    });
    fs::write(&draft, content.to_string()).unwrap();
    let author = |baseline: &str, version: Option<&str>| {
        let mut args = vec![
            "local",
            "author",
            "--ticket",
            "studio",
            "--target",
            "main",
            "--baseline",
            baseline,
            "--draft",
            draft.to_str().unwrap(),
        ];
        if let Some(version) = version {
            args.extend(["--version", version]);
        }
        f.call(&args)
    };
    let parse = |result| serde_json::from_str::<Value>(&ok(result)).unwrap();

    fs::write(f.repo.join("unrelated.txt"), "staged by operator").unwrap();
    git(&f.repo, &["add", "unrelated.txt"]);
    let checkout = git(&f.repo, &["status", "--porcelain"]);
    let first = parse(author(&f.base, None));
    assert_eq!(first["phase"], "plan_pending");
    let spec = first["current"]["spec"].as_str().unwrap();
    assert_eq!(parse(author(&f.base, None))["current"], first["current"]);
    assert_eq!(git(&f.repo, &["status", "--porcelain"]), checkout);
    assert_eq!(git(&f.repo, &["rev-parse", "HEAD"]), f.base);
    assert_eq!(
        git(&f.repo, &["diff", "--name-only", &f.base, spec]),
        "specs/authored/spec.md\nspecs/authored/task.json",
    );
    local_task(
        &f,
        &[
            "approve-plan",
            "--ticket",
            "studio",
            "--version",
            "1",
            "--spec",
            spec,
            "--baseline",
            &f.base,
        ],
    );

    git(&f.repo, &["commit", "-qm", "Advance local target"]);
    let new_base = git(&f.repo, &["rev-parse", "main"]);
    content["spec"] = json!("# Revised\n");
    fs::write(&draft, content.to_string()).unwrap();
    assert!(!author(&f.base, Some("1")).status.success());
    assert_eq!(
        local_task(&f, &["status", "--ticket", "studio"])["phase"],
        "ready"
    );
    let revised = parse(author(&new_base, Some("1")));
    let new_spec = revised["current"]["spec"].as_str().unwrap();
    assert_eq!(revised["current"]["version"], 2);
    assert_eq!(revised["phase"], "plan_pending");
    assert!(revised["history"][0]["plan_approval"].is_object());
    assert_eq!(
        parse(author(&new_base, Some("1")))["current"],
        revised["current"]
    );
    for sha in [spec, new_spec] {
        assert_eq!(
            git(
                &f.repo,
                &["rev-parse", &format!("refs/aep/local-specs/{sha}")]
            ),
            sha
        );
    }
    assert_eq!(
        git(&f.repo, &["show", &format!("{new_spec}:unrelated.txt")]),
        "staged by operator",
    );
    let history = local_task(&f, &["inspect", "--ticket", "studio", "--version", "1"]);
    assert_eq!(history["phase"], "history");
    assert_eq!(history["latest_version"], 2);
    assert_eq!(history["proposal"]["spec"], "# Original\n");
    assert_eq!(
        local_task(&f, &["inspect", "--ticket", "studio"])["proposal"]["spec"],
        "# Revised\n",
    );
    content["spec"] = json!("# Stale editor\n");
    fs::write(&draft, content.to_string()).unwrap();
    assert!(!author(&new_base, Some("1")).status.success());
    assert_eq!(git(&f.repo, &["rev-parse", "HEAD"]), new_base);
    assert!(git(&f.repo, &["status", "--porcelain"]).is_empty());
}

#[test]
fn local_revision_invalidates_plan_and_budget_stops_the_next_client() {
    let f = Fixture::new(&["true"]);
    git(&f.repo, &["switch", "-qc", "revision"]);
    fs::write(f.repo.join("specs/sample/spec.md"), "new requirement").unwrap();
    git(&f.repo, &["commit", "-qam", "new requirement"]);
    let revised = git(&f.repo, &["rev-parse", "HEAD"]);
    git(&f.repo, &["switch", "-q", "main"]);
    f.script(
        "codex",
        r#"
cat >/dev/null
while [ "$1" != --output-last-message ]; do shift; done
printf '{"note":"done","question":""}\n' > "$2"
printf '%s\n' '{"type":"turn.completed","usage":{"input_tokens":8,"output_tokens":2}}'
"#,
    );
    f.script("claude", "touch \"$FIXTURE/claude-started\"; exit 8");
    local_task(
        &f,
        &[
            "plan", "--ticket", "limited", "--task", "sample", "--spec", &f.base,
        ],
    );
    let old = [
        "local",
        "approve-plan",
        "--ticket",
        "limited",
        "--version",
        "1",
        "--spec",
        &f.base,
        "--baseline",
        &f.base,
    ];
    ok(f.call(&old));
    local_task(
        &f,
        &[
            "plan",
            "--ticket",
            "limited",
            "--task",
            "sample",
            "--spec",
            &revised,
            "--max-tokens",
            "10",
            "--revise",
        ],
    );
    assert!(!f.call(&old).status.success());
    local_task(
        &f,
        &[
            "approve-plan",
            "--ticket",
            "limited",
            "--version",
            "2",
            "--spec",
            &revised,
            "--baseline",
            &f.base,
        ],
    );
    assert!(
        !f.call(&["local", "run", "--ticket", "limited"])
            .status
            .success()
    );
    assert!(!f.root.join("claude-started").exists());
    let state = local_task(&f, &["status", "--ticket", "limited"]);
    assert_eq!(state["phase"], "blocked");
    assert_eq!(state["reason"], "token budget exhausted");
    assert!(state["history"][0]["plan_approval"].is_object());
    assert!(
        !f.call(&["local", "run", "--ticket", "limited"])
            .status
            .success()
    );
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), f.base);
}
