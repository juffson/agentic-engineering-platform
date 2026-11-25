use aep::{
    autopilot,
    workflow::{Client, Profile},
};
use anyhow::{Context, Result, ensure};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

/// Claude stand-in with one optional answer file per stage visit.
pub fn fake(dir: &Path) -> Result<Profile> {
    let bin = dir.join("claude");
    fs::write(
        &bin,
        r#"#!/bin/sh
set -eu
[ "${1:-}" = --help ] && exit 0
root=$(dirname "$0")
input=$(cat)
stage=$(printf '%s' "$input" | sed -n 's/^阶段：\([^（]*\)（.*/\1/p' | head -1)
if [ -z "$stage" ]; then
  cat "$root/draft-result.json"
  exit 0
fi
n=$(cat "$root/$stage.count" 2>/dev/null || echo 0)
n=$((n+1))
printf '%s\n' "$n" > "$root/$stage.count"
printf '%s\n' "$@" > "$root/$stage.$n.args"
printf '%s\n' "$input" > "$root/$stage.$n.prompt"
if [ "$stage" = 开发实现 ]; then printf 'backend ready\n' > backend.txt; fi
answer="$stage 完成
AEP: PASS"
if [ -f "$root/$stage.$n" ]; then answer=$(cat "$root/$stage.$n"); fi
printf '%s' "$answer" | python3 -c 'import json,sys; print(json.dumps({"type":"result","result":sys.stdin.read(),"is_error":False}))'
"#,
    )?;
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o700))?;
    Ok(Profile {
        client: Client::Claude,
        executable: bin.display().to_string(),
        model: String::new(),
        check_command: String::new(),
    })
}

pub fn run(home: &Path, pid: &str, wid: &str, until: usize) -> Result<autopilot::State> {
    let output = Command::new(env!("CARGO_BIN_EXE_aep"))
        .args(["workflow-auto", "--home"])
        .arg(home)
        .args([
            "--project",
            pid,
            "--work",
            wid,
            "--until",
            &until.to_string(),
        ])
        .output()?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    autopilot::read(&autopilot::work_dir(home, pid, wid)).context("缺少自动运行状态")
}
