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
        assert_eq!(f.state("locked")["status"], "running");
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
    f.script("claude",r#"prompt=$(cat)
case "$prompt" in *"handoff-719"*) ;; *) exit 1;; esac
case "$prompt" in *log-only-canary*) exit 1;; esac
echo diagnostics >&2
printf '%s' '{"structured_output":{"note":"review-284","question":"","report":"detailed report"}}'"#);
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
