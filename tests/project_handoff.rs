use anyhow::{Result, ensure};
use serde_json::Value;
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn invoke(home: &Path, cwd: &Path, args: &[&str]) -> Result<Output> {
    Ok(Command::new(env!("CARGO_BIN_EXE_aep"))
        .current_dir(cwd)
        .env_remove("AEP_PROJECT")
        .env_remove("AEP_WORK")
        .env_remove("AEP_PROJECT_HOME")
        .args(["project", "--home"])
        .arg(home)
        .args(args)
        .output()?)
}
fn call(home: &Path, cwd: &Path, args: &[&str]) -> Result<Value> {
    let o = invoke(home, cwd, args)?;
    ensure!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    Ok(serde_json::from_slice(&o.stdout)?)
}
#[test]
fn independent_sessions_share_revisions_and_immutable_evidence() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let home = tmp.path().join("library");
    let a = tmp.path().join("design");
    let b = tmp.path().join("build");
    fs::create_dir(&a)?;
    fs::create_dir(&b)?;
    assert!(
        Command::new("git")
            .arg("init")
            .arg("-q")
            .arg(&a)
            .status()?
            .success()
    );
    let api = |args: &[&str]| call(&home, &a, args);
    api(&["create", "product"])?;
    api(&["add-work", "UI"])?;
    api(&["bind"])?;
    api(&["memory", "add", "old acceptance"])?;
    assert!(
        api(&["context"])?["context"]
            .as_str()
            .unwrap()
            .contains("old acceptance")
    );
    assert_eq!(api(&["status"])?["attempts"], serde_json::json!([]));
    let take = [
        "claim",
        "--stage",
        "1",
        "--client",
        "claude",
        "--session",
        "drawing",
    ];
    let claim = api(&take)?;
    let id = claim["attempt"].as_str().unwrap();
    assert_eq!(api(&take)?["attempt"], id);
    assert!(
        !invoke(&home, &b, &["claim", "--stage", "1", "--session", "other"])?
            .status
            .success()
    );
    api(&["memory", "revise", "1", "current acceptance"])?;
    let delta = api(&["context", "--attempt", id, "--since", "1"])?;
    assert_eq!(delta["read_memory"], serde_json::json!([2]));
    assert!(delta["context"].as_str().unwrap().contains("替代 #1"));
    assert_eq!(
        api(&["context", "--attempt", id, "--since", "2"])?["context"],
        ""
    );
    fs::write(a.join("design.md"), "original file")?;
    fs::write(a.join("answer"), "full design result")?;
    let submit = [
        "submit",
        "--attempt",
        id,
        "--session",
        "drawing",
        "--file",
        "answer",
        "--summary",
        "diagram-ready",
        "--artifact",
        "design.md",
    ];
    assert!(
        !invoke(
            &home,
            &a,
            &[
                "submit",
                "--attempt",
                id,
                "--session",
                "someone-else",
                "--file",
                "answer",
                "--summary",
                "bogus"
            ]
        )?
        .status
        .success()
    );
    assert!(
        !invoke(
            &home,
            &a,
            &[
                "submit",
                "--attempt",
                id,
                "--session",
                "drawing",
                "--file",
                "answer",
                "--summary",
                "escape",
                "--artifact",
                "../outside"
            ]
        )?
        .status
        .success()
    );
    let done = api(&submit)?;
    let legacy = api(&["show", "--attempt", id])?;
    assert_eq!(legacy.get("actual_model"), Some(&Value::Null));
    fs::write(a.join("design.md"), "changed later")?;
    assert_eq!(
        invoke(
            &home,
            &a,
            &["show", "--attempt", id, "--artifact", "design.md"]
        )?
        .stdout,
        b"original file"
    );
    assert_eq!(done, api(&submit)?);
    assert_eq!(api(&take)?["status"], "succeeded");
    assert!(
        !invoke(&home, &a, &["confirm-handoff", "--stage", "2", "--run", id])?
            .status
            .success()
    );
    api(&["advance", "--stage", "1"])?;
    let confirmed = api(&["confirm-handoff", "--stage", "1", "--run", id])?;
    assert_eq!(confirmed["run"], id);
    assert_eq!(confirmed["to"], 1);
    assert_eq!(
        confirmed["project_decisions"],
        serde_json::json!([{"id":2,"body":"current acceptance"}])
    );
    assert_eq!(
        api(&["timeline"])?["stage_transfers"],
        serde_json::json!([confirmed])
    );
    assert!(!invoke(&home, &a, &take)?.status.success());
    let next = call(
        &home,
        &b,
        &[
            "claim",
            "--stage",
            "2",
            "--client",
            "zcode",
            "--session",
            "independent",
        ],
    )?;
    assert!(next["context"].as_str().unwrap().contains("diagram-ready"));
    assert_eq!(next["workspace"], b.canonicalize()?.to_str().unwrap());
    assert_eq!(
        call(&home, &b, &["status"])?["stages"][1]["profile"]["client"],
        "Claude"
    );
    let second = next["attempt"].as_str().unwrap();
    let receipt = api(&["show", "--attempt", second, "--part", "receipt"])?;
    assert_eq!(receipt["read_stage_transfer"], confirmed);
    assert!(
        next["context"]
            .as_str()
            .unwrap()
            .contains(confirmed["acceptance"].as_str().unwrap())
    );
    api(&["cancel", "--attempt", second])?;
    assert!(
        !invoke(
            &home,
            &a,
            &["confirm-handoff", "--stage", "2", "--run", second]
        )?
        .status
        .success()
    );
    fs::write(b.join("still-owned"), "own session is usable")?;
    assert!(
        !invoke(
            &home,
            &a,
            &[
                "submit",
                "--attempt",
                second,
                "--session",
                "independent",
                "--file",
                "answer",
                "--summary",
                "late"
            ]
        )?
        .status
        .success()
    );
    assert!(
        !invoke(&home, &a, &["advance", "--stage", "2"])?
            .status
            .success()
    );
    // The registered checkout stays bound to its own work while a different project is active.
    call(&home, &b, &["create", "unrelated"])?;
    assert_eq!(api(&["status"])?["name"], "UI");
    Ok(())
}

#[test]
fn delta_pages_do_not_skip_revisions_when_the_packet_fills() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let home = tmp.path();
    let api = |args: &[&str]| call(home, home, args);
    api(&["create", "batch"])?;
    api(&["add-work", "docs"])?;
    let c = api(&["claim", "--stage", "1", "--session", "active"])?;
    let id = c["attempt"].as_str().unwrap();
    let note = "决策".repeat(160);
    for _ in 0..36 {
        api(&["memory", "add", &note])?;
    }
    let mut cursor = 0;
    let mut reads = vec![];
    loop {
        let packet = api(&["context", "--attempt", id, "--since", &cursor.to_string()])?;
        assert!(packet["context"].as_str().unwrap().len() <= 1600);
        reads.extend(
            packet["read_memory"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_i64().unwrap()),
        );
        cursor = packet["next_since"].as_i64().unwrap();
        if packet["has_more"] != true {
            break;
        }
    }
    assert_eq!(reads, (1..=36).collect::<Vec<_>>());
    Ok(())
}

#[test]
fn native_zcode_bridge_uses_the_existing_default_and_keeps_its_session_id() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir()?;
    let path = temp.path();
    let binary = path.join("zcode");
    fs::write(
        &binary,
        r#"#!/bin/sh
set -eu
test "$1" = app-server
IFS= read -r create
sid=$(printf '%s' "$create" | sed -n 's/.*"sessionId":"\([^"]*\)".*/\1/p')
printf '%s\n' '{"id":1,"result":{"settings":{"model":{"current":{"providerId":"native","modelId":"local-default"}}}}}'
IFS= read -r subscribe
printf '%s\n' '{"id":2,"result":{}}'
IFS= read -r prompt
case "$prompt" in *'"modelSelection":'*) exit 9;; esac
printf '%s\n' '{"id":3,"result":{"accepted":true}}'
printf '{"method":"session/event","params":{"sessionId":"%s","type":"turn.completed"}}\n' "$sid"
IFS= read -r messages
printf '%s\n' '{"id":4,"result":{"messages":[{"info":{"role":"assistant"},"parts":[{"type":"text","text":"native bridge ready"}]}]}}'
"#,
    )?;
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700))?;
    let home = path.join("library");
    call(&home, path, &["create", "ZCode project"])?;
    call(&home, path, &["add-work", "ZCode work"])?;
    let mut library = aep::project::Library::open(&home)?;
    library.change(&home, |library| {
        library
            .project_mut()?
            .set_zcode_default("other/project-default".into());
        Ok(())
    })?;
    let out = Command::new(env!("CARGO_BIN_EXE_aep"))
        .current_dir(path)
        .env("HOME", path)
        .env_remove("AEP_PROJECT")
        .env_remove("AEP_WORK")
        .args(["project", "--home"])
        .arg(&home)
        .args(["run", "--client", "zcode", "--executable"])
        .arg(&binary)
        .output()?;
    ensure!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let evidence: Value = serde_json::from_slice(&out.stdout)?;
    let view = call(&home, path, &["status"])?;
    let attempt = &view["attempts"][0];
    let id = attempt["id"].as_str().unwrap();
    let dir = aep::workflow::attempt_dir(
        &home,
        view["project"].as_str().unwrap(),
        view["work"].as_str().unwrap(),
        attempt["id"].as_str().unwrap(),
    );
    assert_eq!(
        fs::read_to_string(dir.join("result.txt"))?,
        "native bridge ready\n"
    );
    let session = aep::workspace::read(&dir.join("session.json"))?;
    assert_eq!(session["transport"], "app-server");
    assert!(session["session"].as_str().unwrap().starts_with("sess_"));
    assert_eq!(session["providerId"], "native");
    assert_eq!(session["modelId"], "local-default");
    assert_eq!(session["selected"], "");
    assert_eq!(session["reasoningLevel"], Value::Null);
    assert_eq!(session["source"], "settings");
    assert_eq!(
        session,
        aep::workspace::read(&dir.join("zcode-model.json"))?
    );
    assert_eq!(attempt["model"], "");
    assert_eq!(attempt["actual_model"], evidence["actual_model"]);
    assert_eq!(
        call(&home, path, &["show", "--attempt", id])?["actual_model"],
        evidence["actual_model"]
    );
    assert_eq!(attempt["actual_model"]["modelId"], "local-default");
    let note = evidence["written_memory"].as_i64().unwrap().to_string();
    assert_eq!(
        call(&home, path, &["memory", "get", &note])?["author"],
        "ZCode / native / local-default@默认 · 客户端默认"
    );
    let mismatch = invoke(
        &home,
        path,
        &[
            "run",
            "--client",
            "zcode",
            "--executable",
            binary.to_str().unwrap(),
            "--model",
            "native/other",
        ],
    )?;
    assert!(!mismatch.status.success());
    let failed: Value = serde_json::from_slice(&mismatch.stdout)?;
    let error = failed["error"].as_str().unwrap();
    assert!(error.contains("所选 native/other") && error.contains("实际 native / local-default"));
    assert!(failed["written_memory"].is_null());
    assert!(failed["actual_model"].is_null());
    Ok(())
}

#[test]
fn directed_handoffs_and_site_overrides_are_scoped_to_the_selected_project() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let repo = tmp.path().join("repo");
    let home = tmp.path().join("library");
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .arg(&repo)
            .status()?
            .success()
    );
    let api = |args: &[&str]| call(&home, &repo, args);
    api(&["create", "first", "--repository", "."])?;
    api(&["add-work", "source"])?;
    api(&["add-work", "target"])?;
    api(&[
        "--work",
        "source",
        "handoff",
        "--to",
        "target",
        "--note",
        "superseded message",
    ])?;
    api(&[
        "--work",
        "source",
        "handoff",
        "--to",
        "target",
        "--note",
        "current decision",
    ])?;
    let handoffs = api(&["--work", "target", "status"])?;
    assert_eq!(handoffs["handoffs"].as_array().unwrap().len(), 1);
    let claim = api(&[
        "--work",
        "target",
        "claim",
        "--stage",
        "1",
        "--session",
        "reader",
    ])?;
    let packet = claim["context"].as_str().unwrap();
    assert!(packet.contains("current decision") && !packet.contains("superseded message"));
    assert!(
        !api(&["--work", "source", "context"])?["context"]
            .as_str()
            .unwrap()
            .contains("current decision")
    );
    let operator = tmp.path().join("operator");
    fs::write(&operator, "fixture-only-token-content")?;
    let token_path = operator.to_str().unwrap();
    api(&[
        "site",
        "--repository",
        ".",
        "--endpoint",
        "https://localhost:4242",
        "--operator-file",
        token_path,
    ])?;
    assert!(
        !invoke(
            &home,
            &repo,
            &[
                "site",
                "--repository",
                ".",
                "--endpoint",
                "https://localhost:4242/wrong",
                "--operator-file",
                token_path
            ]
        )?
        .status
        .success()
    );
    let record = aep::workspace::Workspace::open(&repo)?
        .data
        .join("relays/relay-old");
    fs::create_dir_all(&record)?;
    fs::write(record.join("relay.json"), "{}")?;
    assert!(
        !invoke(
            &home,
            &repo,
            &[
                "--work",
                "source",
                "link-record",
                "--repository",
                ".",
                "relay",
                "relay-old"
            ]
        )?
        .status
        .success()
    );
    fs::write(
        record.join("relay.json"),
        br#"{"id":"relay-old","task":{"name":"archived"},"attempts":[]}"#,
    )?;
    api(&[
        "--work",
        "source",
        "link-record",
        "--repository",
        ".",
        "relay",
        "relay-old",
    ])?;
    assert!(
        !invoke(
            &home,
            &repo,
            &[
                "--work",
                "target",
                "link-record",
                "--repository",
                ".",
                "relay",
                "relay-old"
            ]
        )?
        .status
        .success()
    );
    api(&[
        "--work",
        "source",
        "unlink-record",
        "--repository",
        ".",
        "relay",
        "relay-old",
    ])?;
    assert!(record.join("relay.json").is_file());
    api(&["create", "second", "--repository", "."])?;
    api(&["add-work", "target"])?;
    assert!(api(&["site", "--repository", "."])?.is_null());
    assert!(
        !api(&["context"])?["context"]
            .as_str()
            .unwrap()
            .contains("current decision")
    );
    assert_eq!(
        api(&["--project", "first", "site", "--repository", "."])?["endpoint"],
        "https://localhost:4242"
    );
    assert!(
        !fs::read_to_string(home.join("projects.json"))?.contains("fixture-only-token-content")
    );
    Ok(())
}

#[test]
fn status_summarizes_https_records() -> Result<()> {
    use serde_json::json;
    let tmp = tempfile::tempdir()?;
    let home = tmp.path().join("library");
    let repo = tmp.path().join("repo");
    fs::create_dir(&repo)?;
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .arg(&repo)
            .status()?
            .success()
    );
    let repo = repo.canonicalize()?;
    let api = |args: &[&str]| call(&home, &repo, args);
    api(&["create", "summary", "--repository", "."])?;
    api(&["add-work", "other"])?;
    api(&["add-work", "current"])?;
    assert_eq!(api(&["status"])?["https"], json!([]));
    let parent = aep::workspace::Workspace::open(&repo)?
        .data
        .join("https-studio");
    let operator = tmp.path().join("operator-token");
    let ca = tmp.path().join("private-ca");
    fs::write(&operator, "fixture-token-content")?;
    fs::write(&ca, "fixture-ca-content")?;
    let request = json!({
        "task":{"name":"task-one","secret":"nested-request-secret"},
        "branch":{"name":"feature/one"}, "ticket":"https-task-one",
        "endpoint":"https://localhost:4242/", "operator_file":operator, "ca":ca,
        "extra":"request-secret"
    });
    let state = json!({"stage":"remote_checking","exit_code":1,"pr":42,
        "reason":"验收未通过", "extra":"state-secret", "task":"wrong-task"});
    let ids = [
        "studio-good",
        "studio-pending",
        "studio-bad-request",
        "studio-bad-state",
        "studio-missing",
        "studio-site",
        "studio-token",
        "studio-ca",
        "studio-long",
        "studio-unlinked",
        "studio-other",
    ];
    for id in ids {
        let dir = parent.join(id);
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("request.json"), serde_json::to_vec(&request)?)?;
        fs::write(dir.join("state.json"), serde_json::to_vec(&state)?)?;
        if id == "studio-unlinked" {
            continue;
        }
        let work = if id == "studio-other" {
            "other"
        } else {
            "current"
        };
        api(&[
            "--work",
            work,
            "link-record",
            "--repository",
            ".",
            "https",
            id,
        ])?;
    }
    fs::remove_file(parent.join("studio-pending/state.json"))?;
    fs::write(parent.join("studio-bad-request/request.json"), b"{broken")?;
    fs::write(parent.join("studio-bad-state/state.json"), b"[]")?;
    fs::remove_dir_all(parent.join("studio-missing"))?;
    for (id, secret) in [
        ("studio-site", "localhost:4242"),
        ("studio-token", operator.to_str().unwrap()),
        ("studio-ca", ca.to_str().unwrap()),
    ] {
        let mut value = state.clone();
        // Sensitive content after the display limit must still hide the entire reason.
        value["reason"] = json!(format!("{} {secret}", "错".repeat(205)));
        fs::write(
            parent.join(id).join("state.json"),
            serde_json::to_vec(&value)?,
        )?;
    }
    let mut long = state.clone();
    long["reason"] = json!("错".repeat(205));
    fs::write(
        parent.join("studio-long/state.json"),
        serde_json::to_vec(&long)?,
    )?;
    // Persisted historical references can be invalid; status must not follow them.
    let catalog = home.join("projects.json");
    let mut library: Value = serde_json::from_slice(&fs::read(&catalog)?)?;
    let records = library["projects"][0]["work"][1]["records"]
        .as_array_mut()
        .unwrap();
    records.push(json!({"repository":repo,"kind":"https","id":"../studio-unlinked"}));
    records.push(json!({"repository":repo,"kind":"relay","id":"relay-ignored"}));
    fs::write(&catalog, serde_json::to_vec(&library)?)?;
    let before = fs::read(&catalog)?;
    let original_request = fs::read(parent.join("studio-good/request.json"))?;
    let original_state = fs::read(parent.join("studio-good/state.json"))?;
    let status = api(&["status"])?;
    assert_eq!(
        status["records"],
        library["projects"][0]["work"][1]["records"]
    );
    let summaries = status["https"].as_array().unwrap();
    assert_eq!(summaries.len(), 10);
    let entry = |id: &str| summaries.iter().find(|s| s["id"] == id).unwrap();
    assert_eq!(
        entry("studio-good"),
        &json!({"id":"studio-good","repository":repo,
        "task":"task-one","branch":"feature/one","ticket":"https-task-one",
        "stage":"remote_checking","exit_code":1,"pr":42,"reason":"验收未通过"})
    );
    assert_eq!(
        entry("studio-pending"),
        &json!({"id":"studio-pending","repository":repo,
        "task":"task-one","branch":"feature/one","ticket":"https-task-one"})
    );
    for id in [
        "studio-bad-request",
        "studio-bad-state",
        "../studio-unlinked",
    ] {
        assert_eq!(entry(id), &json!({"id":id,"error":"corrupt"}));
    }
    assert_eq!(
        entry("studio-missing"),
        &json!({"id":"studio-missing","error":"missing"})
    );
    for id in ["studio-site", "studio-token", "studio-ca"] {
        assert_eq!(entry(id)["reason"], "已隐藏");
    }
    assert_eq!(entry("studio-long")["reason"], "错".repeat(200));
    let output = serde_json::to_string(&status)?;
    for secret in [
        "localhost:4242",
        operator.to_str().unwrap(),
        ca.to_str().unwrap(),
        "operator_file",
        "fixture-token-content",
        "request-secret",
        "state-secret",
        "nested-request-secret",
    ] {
        assert!(!output.contains(secret));
    }
    assert_eq!(fs::read(&catalog)?, before);
    assert_eq!(
        fs::read(parent.join("studio-good/request.json"))?,
        original_request
    );
    assert_eq!(
        fs::read(parent.join("studio-good/state.json"))?,
        original_state
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn status_summarizes_https_records_confines_symlinks() -> Result<()> {
    use aep::project::RecordKind;
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir()?;
    let repo = tmp.path().join("repo");
    fs::create_dir(&repo)?;
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .arg(&repo)
            .status()?
            .success()
    );
    let data = aep::workspace::Workspace::open(&repo)?.data;
    let parent = data.join("https-studio");
    fs::create_dir_all(&parent)?;
    let outside = tmp.path().join("outside");
    fs::create_dir(&outside)?;
    fs::write(
        outside.join("request.json"),
        br#"{"task":{"name":"outside"},"branch":{"name":"outside"}}"#,
    )?;
    symlink(&outside, parent.join("studio-dir"))?;
    // A link to a different record inside the same archive also crosses the item boundary.
    let sibling = parent.join("studio-other");
    fs::create_dir(&sibling)?;
    fs::copy(outside.join("request.json"), sibling.join("request.json"))?;
    symlink(&sibling, parent.join("studio-alias"))?;
    let dir = parent.join("studio-file");
    fs::create_dir(&dir)?;
    symlink(outside.join("request.json"), dir.join("request.json"))?;
    for id in ["studio-dir", "studio-file", "studio-alias"] {
        assert_eq!(
            RecordKind::Https.summary(&repo, id),
            serde_json::json!({"id":id,"error":"corrupt"})
        );
    }
    fs::remove_file(dir.join("request.json"))?;
    fs::copy(outside.join("request.json"), dir.join("request.json"))?;
    fs::write(outside.join("state.json"), b"{}")?;
    symlink(outside.join("state.json"), dir.join("state.json"))?;
    assert_eq!(
        RecordKind::Https.summary(&repo, "studio-file")["error"],
        "corrupt"
    );
    fs::remove_dir_all(&parent)?;
    symlink(&outside, &parent)?;
    assert_eq!(
        RecordKind::Https.summary(&repo, "studio-dir")["error"],
        "corrupt"
    );
    Ok(())
}

#[test]
fn directory_opens_the_cli_bound_catalog_and_item_without_duplicate_writes() -> Result<()> {
    use aep::project::directory::{Binding, DirectoryAction, DirectoryProject};
    use aep::workspace::Workspace;
    let tmp = tempfile::tempdir()?;
    let repo = tmp.path().join("sources");
    let home = tmp.path().join("session-library");
    let other = tmp.path().join("studio-library");
    ensure!(
        Command::new("git")
            .args(["init", "-q"])
            .arg(&repo)
            .status()?
            .success()
    );
    let api = |args: &[&str]| call(&home, &repo, args);
    let project = api(&["create", "bound", "--repository", "."])?["project"].clone();
    let work = api(&["add-work", "review"])?["work"].clone();
    api(&["bind"])?;
    api(&["add-work", "another"])?;
    api(&["create", "last opened"])?;
    let dir = repo.join("src");
    fs::create_dir(&dir)?;
    let ws = Workspace::open(&dir)?;
    let restored = DirectoryProject::resolve(&ws, &other, None, DirectoryAction::Restore)?;
    assert_eq!(restored.library.project().unwrap().name, "last opened");
    assert!(!restored.bound_work);
    let opened = DirectoryProject::resolve(&ws, &other, None, DirectoryAction::Open)?;
    assert!(opened.bound_work);
    assert_eq!(opened.home, home);
    assert_eq!(opened.library.active.as_deref(), project.as_str());
    assert_eq!(
        opened.library.project().unwrap().selected.as_deref(),
        work.as_str()
    );
    let revision = opened.library.revision;
    assert_eq!(
        DirectoryProject::resolve(&ws, &other, None, DirectoryAction::Open)?
            .library
            .revision,
        revision
    );
    assert!(!other.exists());
    let explicit = DirectoryProject::resolve(&ws, &other, Some(&other), DirectoryAction::Open)?;
    assert!(!explicit.bound_work);
    assert_eq!(explicit.home, other);
    let file = Binding::file(&repo).unwrap();
    let mut stale = Binding::read(&file)?.unwrap();
    stale.work = "missing".into();
    fs::write(&file, serde_json::to_vec(&stale)?)?;
    assert!(DirectoryProject::resolve(&ws, &other, None, DirectoryAction::Open).is_err());
    assert_eq!(aep::project::Library::open(&home)?.revision, revision);
    Ok(())
}

#[test]
fn directory_matches_linked_worktrees_without_guessing_between_projects() -> Result<()> {
    use aep::project::directory::{DirectoryAction, DirectoryProject};
    use aep::workspace::{Workspace, git};
    let tmp = tempfile::tempdir()?;
    let home = tmp.path().join("catalog");
    let repo = tmp.path().join("repo");
    let tree = tmp.path().join("tree");
    fs::create_dir(&repo)?;
    git(&repo, &["init", "-q"])?;
    git(
        &repo,
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.test",
            "commit",
            "--allow-empty",
            "-qm",
            "fixture",
        ],
    )?;
    git(
        &repo,
        &["worktree", "add", "-q", "--detach", tree.to_str().unwrap()],
    )?;
    let api = |args: &[&str]| call(&home, &repo, args);
    api(&["create", "first", "--repository", "."])?;
    api(&["create", "unrelated"])?;
    let ws = Workspace::open(&tree)?;
    assert_eq!(
        DirectoryProject::resolve(&ws, &home, None, DirectoryAction::Open)?
            .library
            .project()
            .unwrap()
            .name,
        "first"
    );
    api(&["create", "second", "--repository", "."])?;
    assert_eq!(
        DirectoryProject::resolve(&ws, &home, None, DirectoryAction::Open)?
            .library
            .project()
            .unwrap()
            .name,
        "second"
    );
    api(&["--project", "unrelated", "open"])?;
    assert!(DirectoryProject::resolve(&ws, &home, None, DirectoryAction::Open).is_err());
    let unlinked = tmp.path().join("new");
    fs::create_dir(&unlinked)?;
    git(&unlinked, &["init", "-q"])?;
    assert!(
        DirectoryProject::resolve(
            &Workspace::open(&unlinked)?,
            &home,
            None,
            DirectoryAction::Open
        )
        .is_err()
    );
    assert_eq!(
        aep::project::Library::open(&home)?.project().unwrap().name,
        "unrelated"
    );
    Ok(())
}

#[test]
fn a_follow_up_resumes_the_claude_session_with_only_the_new_message() -> Result<()> {
    use aep::{
        handoff::Assignment,
        project::{Kind, Library},
        workflow::{Client, Profile, Template, attempt_dir},
    };
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("catalog");
    let claude = temp.path().join("claude");
    // First turn: a fresh stream-json session. Follow-up: must resume s-1 and receive only the message.
    fs::write(
        &claude,
        r#"#!/bin/sh
set -eu
input=$(cat)
case " $* " in
  *" --resume s-1 "*)
    test "$input" = "请补充回滚方案"
    printf '%s\n' '{"type":"system","subtype":"init","session_id":"s-1","model":"sonnet"}'
    printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"text","text":"已补充回滚方案"}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","result":"已补充回滚方案","is_error":false,"session_id":"s-1"}'
    ;;
  *"--resume"*) exit 7 ;;
  *)
    case " $* " in *" stream-json --verbose "*) ;; *) exit 6 ;; esac
    printf '%s\n' '{"type":"system","subtype":"init","session_id":"s-1","model":"sonnet"}'
    printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"u1","name":"Bash","input":{"command":"ls"}}]}}'
    printf '%s\n' '{"type":"user","message":{"content":[{"tool_use_id":"u1","type":"tool_result","content":"a.txt","is_error":false}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","result":"初版方案","is_error":false,"session_id":"s-1"}'
    ;;
esac
"#,
    )?;
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o700))?;
    let profile = Profile {
        client: Client::Claude,
        executable: claude.display().to_string(),
        model: String::new(),
        check_command: String::new(),
    };
    let mut lib = Library::open(&home)?;
    let first = lib.change(&home, |lib| {
        lib.create("follow", vec![])?;
        let p = lib.project_mut()?;
        p.add_work("方案", Kind::Feature, Template::Software)?;
        let work = p.selected().unwrap().id.clone();
        p.reserve(
            &home,
            &work,
            0,
            Assignment {
                profile: profile.clone(),
                brief: "写一版方案".into(),
                repository: None,
                external: None,
            },
        )
    })?;
    let stage = |dir: &Path| -> Result<Value> {
        let out = Command::new(env!("CARGO_BIN_EXE_aep"))
            .args(["workflow-stage", "--request"])
            .arg(dir.join("request.json"))
            .output()?;
        ensure!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(serde_json::from_slice(&fs::read(
            dir.join("evidence.json"),
        )?)?)
    };
    let ev = stage(&first)?;
    assert_eq!(ev["session"], "s-1");
    assert_eq!(fs::read_to_string(first.join("result.txt"))?, "初版方案");
    let (pid, wid) = {
        let p = lib.project().unwrap();
        (p.id.clone(), p.selected().unwrap().id.clone())
    };
    let next = lib.change(&home, |lib| {
        lib.project_mut()?
            .reserve_follow_up(&home, &wid, 0, "请补充回滚方案")
    })?;
    let ev = stage(&next)?;
    assert_eq!(ev["status"], "succeeded", "{ev}");
    assert_eq!(ev["follow_up"]["resumed_session"], "s-1");
    let before: Value = serde_json::from_slice(&fs::read(first.join("evidence.json"))?)?;
    assert_eq!(
        ev["workspace"], before["workspace"],
        "follow-up must reuse the workspace"
    );
    assert_eq!(
        fs::read_to_string(next.join("result.txt"))?,
        "已补充回滚方案"
    );
    let p = lib.project().unwrap();
    let work = p.selected().unwrap();
    assert_eq!(
        work.notes[0], "写一版方案",
        "follow-up must not replace the stage note"
    );
    let current = &work.current_attempt[&0];
    assert_eq!(attempt_dir(&home, &pid, &wid, current), next);
    let t = aep::transcript::parse(&fs::read_to_string(first.join("stdout.log"))?);
    assert!(
        matches!(&t.entries[0], aep::transcript::Entry::Command { command, exit: Some(0), .. } if command == "ls")
    );
    Ok(())
}

#[test]
fn a_failed_gate_can_loop_back_to_build_and_never_strands_the_item() -> Result<()> {
    use aep::{
        handoff::Assignment,
        project::{Kind, Library},
        workflow::{Client, Profile, Template},
    };
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("catalog");
    let claude = temp.path().join("claude");
    fs::write(
        &claude,
        "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"type\":\"result\",\"result\":\"端口被沙箱拒绝\\nAEP: BLOCKED\",\"is_error\":false,\"session_id\":\"s-9\"}'\n",
    )?;
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o700))?;
    let profile = Profile {
        client: Client::Claude,
        executable: claude.display().to_string(),
        model: String::new(),
        check_command: String::new(),
    };
    let mut lib = Library::open(&home)?;
    let wid = lib.change(&home, |lib| {
        lib.create("loop", vec![])?;
        let p = lib.project_mut()?;
        p.add_work("导出", Kind::Feature, Template::Software)?;
        let wid = p.selected().unwrap().id.clone();
        for stage in 0..4 {
            p.finish_stage(&home, &wid, stage, format!("人工结论 {stage}"))?;
        }
        Ok(wid)
    })?;
    let run_gate = |lib: &mut Library| -> Result<()> {
        let dir = lib.change(&home, |lib| {
            lib.project_mut()?.reserve(
                &home,
                &wid,
                4,
                Assignment {
                    profile: profile.clone(),
                    brief: "跑全量测试".into(),
                    repository: None,
                    external: None,
                },
            )
        })?;
        Command::new(env!("CARGO_BIN_EXE_aep"))
            .args(["workflow-stage", "--request"])
            .arg(dir.join("request.json"))
            .output()?;
        let ev: Value = serde_json::from_slice(&fs::read(dir.join("evidence.json"))?)?;
        ensure!(ev["status"] == "failed", "{ev}");
        Ok(())
    };
    run_gate(&mut lib)?;
    // A failed gate cannot be advanced as if it passed.
    assert!(
        lib.change(&home, |lib| lib.project_mut()?.finish_stage(
            &home,
            &wid,
            4,
            String::new()
        ))
        .is_err()
    );
    // Loop: back to build, then forward again into a fresh test round.
    lib.change(&home, |lib| {
        lib.project_mut()?
            .work_by_id_mut(&wid)?
            .send_back(3, "端口被沙箱拒绝")
    })?;
    {
        let work = &lib.project().unwrap().work[0];
        assert_eq!(work.stage, 3);
        assert!(!work.current_attempt.contains_key(&4));
        assert_eq!(work.reworks_between(4, 3), 1);
    }
    lib.change(&home, |lib| {
        lib.project_mut()?
            .finish_stage(&home, &wid, 3, "已修复".into())
    })?;
    run_gate(&mut lib)?;
    // The way out when the failure is environmental: a recorded human decision.
    assert!(
        lib.change(&home, |lib| lib
            .project_mut()?
            .waive_stage(&home, &wid, 4, " "))
            .is_err(),
        "waiving needs a reason"
    );
    lib.change(&home, |lib| {
        lib.project_mut()?
            .waive_stage(&home, &wid, 4, "本机手动跑通")
    })?;
    let work = &lib.project().unwrap().work[0];
    assert_eq!(work.stage, 5);
    assert!(work.notes[4].starts_with("人工放行：本机手动跑通"));
    assert!(work.notes[4].contains("AEP: BLOCKED"));
    Ok(())
}

#[test]
fn a_follow_up_falls_back_to_the_previous_answer_when_the_session_is_gone() -> Result<()> {
    use aep::{
        handoff::Assignment,
        project::{Kind, Library},
        workflow::{Client, Profile, Template},
    };
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("catalog");
    let claude = temp.path().join("claude");
    // Resuming always fails as if the session was pruned; a replayed prompt must carry
    // the earlier answer and the new request.
    fs::write(
        &claude,
        r#"#!/bin/sh
input=$(cat)
case " $* " in *" --resume "*) echo 'No conversation found with session ID: s-1' >&2; exit 1 ;; esac
case "$input" in
  *"继续的要求"*)
    case "$input" in *"上一轮答复"*"初版方案"*"请直接修改"*|*"上一轮答复"*"已按要求修改"*"请直接修改"*) ;; *) exit 5 ;; esac
    printf '%s\n' '{"type":"result","result":"已按要求修改","is_error":false,"session_id":"s-2"}'
    ;;
  *) printf '%s\n' '{"type":"result","result":"初版方案","is_error":false,"session_id":"s-1"}' ;;
esac
"#,
    )?;
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o700))?;
    let profile = Profile {
        client: Client::Claude,
        executable: claude.display().to_string(),
        model: String::new(),
        check_command: String::new(),
    };
    let stage = |dir: &Path| -> Result<Value> {
        Command::new(env!("CARGO_BIN_EXE_aep"))
            .args(["workflow-stage", "--request"])
            .arg(dir.join("request.json"))
            .output()?;
        Ok(serde_json::from_slice(&fs::read(
            dir.join("evidence.json"),
        )?)?)
    };
    let mut lib = Library::open(&home)?;
    let (wid, first) = lib.change(&home, |lib| {
        lib.create("gone", vec![])?;
        let p = lib.project_mut()?;
        p.add_work("方案", Kind::Feature, Template::Software)?;
        let wid = p.selected().unwrap().id.clone();
        let dir = p.reserve(
            &home,
            &wid,
            0,
            Assignment {
                profile: profile.clone(),
                brief: "写一版方案".into(),
                repository: None,
                external: None,
            },
        )?;
        Ok((wid, dir))
    })?;
    assert_eq!(stage(&first)?["resumable"], true);
    let next = lib.change(&home, |lib| {
        lib.project_mut()?
            .reserve_follow_up(&home, &wid, 0, "请直接修改")
    })?;
    let ev = stage(&next)?;
    assert_eq!(ev["status"], "succeeded", "{ev}");
    assert!(ev["follow_up"]["resume_error"].is_string(), "{ev}");
    assert_eq!(fs::read_to_string(next.join("result.txt"))?, "已按要求修改");

    // A session recorded by an older build (no `resumable` marker) is never resumed.
    for dir in [&first, &next] {
        let mut old: Value = serde_json::from_slice(&fs::read(dir.join("evidence.json"))?)?;
        old.as_object_mut().unwrap().remove("resumable");
        fs::write(dir.join("evidence.json"), serde_json::to_vec(&old)?)?;
    }
    let third = lib.change(&home, |lib| {
        lib.project_mut()?
            .reserve_follow_up(&home, &wid, 0, "请直接修改")
    })?;
    let ev = stage(&third)?;
    assert_eq!(ev["status"], "succeeded", "{ev}");
    assert!(
        ev["follow_up"]["resume_error"].is_null(),
        "should not try to resume: {ev}"
    );
    Ok(())
}
