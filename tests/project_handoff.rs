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
    api(&["advance", "--stage", "1"])?;
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
    api(&["cancel", "--attempt", second])?;
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
case "$create" in *'"model":'*) exit 8;; esac
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
    fs::write(
        path.join("request.json"),
        serde_json::to_vec(
            &serde_json::json!({"project":"p","work":"w","template":"Software","stage":0,"profile":{"client":"ZCode","model":"","executable":""},"repository":null,"brief":"test","approved":[]}),
        )?,
    )?;
    fs::write(path.join("prompt.txt"), "native default")?;
    let out = Command::new(env!("CARGO_BIN_EXE_aep"))
        .current_dir(path)
        .args(["zcode-stage", "--binary"])
        .arg(binary)
        .args(["--request", "request.json"])
        .output()?;
    ensure!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, b"native bridge ready\n");
    let session: Value = serde_json::from_slice(&fs::read(path.join("session.json"))?)?;
    assert_eq!(session["transport"], "app-server");
    assert!(session["session"].as_str().unwrap().starts_with("sess_"));
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
