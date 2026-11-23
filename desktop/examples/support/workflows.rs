//! Exercise the Studio-to-CLI boundary with deterministic local clients.
use super::{Args, Driver, fixture, handoff_fixture, https_queue::until, project::click};
use aep::{workflow::attempt_dir, workspace::read};
use anyhow::{Context, Result, ensure};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

fn client(bin: &Path, name: &str, body: &str) -> Result<()> {
    let path = bin.join(name);
    fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n"))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}
pub(super) fn run(args: &Args) -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let repo = tmp.path().join("repo");
    let bin = tmp.path().join("bin");
    fs::create_dir(&repo)?;
    fs::create_dir(&bin)?;
    let (ws, _) = fixture(&repo)?;
    client(
        &bin,
        "claude",
        r#"input=$(cat)
case "$input" in *"studio-memory-new"*) ;; *) echo 'missing revised decision' >&2;exit 9;; esac
printf '%s\n' '{"result":"handoff from Claude: frost-83\nAEP: PASS","is_error":false}'"#,
    )?;
    client(
        &bin,
        "codex",
        r#"input=$(cat)
case "$input" in *frost-83*) ;; *) echo 'missing handoff' >&2; exit 6;; esac
while [ "$1" != "--output-last-message" ]; do shift; done
case "$input" in *"阶段职责：在事项工作区完成"*) echo local-build > gameplay.txt;; esac
printf 'handoff from Codex: frost-83\nAEP: PASS\n' > "$2""#,
    )?;
    client(
        &bin,
        "zcode",
        r#"if [ "$1" = "app-server" ]; then
    read -r create
    case "$create" in *'"modelId":"qa-model"'*) ;; *) exit 7;; esac
    sid=$(printf '%s' "$create" | sed -n 's/.*"sessionId":"\([^"]*\)".*/\1/p')
    printf '%s\n' '{"id":1,"result":{"settings":{"model":{"current":{"providerId":"team","modelId":"qa-model"}}}}}'
    read -r subscribe
    printf '%s\n' '{"id":2,"result":{}}'
    read -r prompt
    case "$prompt" in *frost-83*) ;; *) exit 8;; esac
    printf '%s\n' '{"id":3,"result":{"accepted":true}}'
    printf '{"method":"session/event","params":{"sessionId":"%s","type":"turn.completed"}}\n' "$sid"
    read -r messages
    printf '%s\n' '{"id":4,"result":{"messages":[{"info":{"role":"assistant"},"parts":[{"type":"text","text":"game checks passed, frost-83\nAEP: PASS"}]}]}}'
    exit 0
fi
while [ "$1" != "--attach" ]; do shift; done
grep -q frost-83 "$2"
if [ ! -e already-tested ]; then touch already-tested; echo 'simulated missing engine' >&2; exit 12; fi
printf 'game checks passed, frost-83\nAEP: PASS\n'"#,
    )?;
    client(
        &bin,
        "delivery-cli",
        r#"input=$(cat)
case "$input" in *frost-83*) ;; *) exit 8;; esac
if [ ! -e delivery-started ]; then touch delivery-started; sleep 120; fi
echo "已核对玩家验收记录，交付 frost-83。""#,
    )?;
    let runner = aep_studio::Runner {
        binary: Some(handoff_fixture::build_runner()?),
        client_bin: Some(bin),
    };
    let mut ui = Driver::open_with_runner(&ws, args, runner.clone())?;
    let catalog = ws.data.join("studio-projects");
    let result = (|| -> Result<()> {
        ui.click("back-to-project")?;
        ui.click("project-name")?;
        ui.input("游戏工作室")?;
        ui.click("project-create")?;
        ui.click("project-repos")?;
        ui.click("project-repo-path")?;
        ui.input(repo.to_str().unwrap())?;
        ui.click("project-add-path")?;
        ui.click("project-memory")?;
        click(&mut ui, "project-memory-text")?;
        ui.input("studio-memory-old")?;
        click(&mut ui, "project-memory-save")?;
        click(&mut ui, "project-memory-revise-1")?;
        click(&mut ui, "project-memory-text")?;
        ui.replace_focused("studio-memory-new")?;
        click(&mut ui, "project-memory-save")?;
        ui.click("project-overview")?;
        ui.click("project-new-work")?;
        ui.click("work-template-Game")?;
        ui.click("project-work-name")?;
        ui.input("玩家存档恢复")?;
        ui.click("project-create-work")?;
        let state = read(&catalog.join("projects.json"))?;
        let p = &state["projects"][0];
        let project = p["id"].as_str().unwrap();
        let work = p["work"][0]["id"].as_str().unwrap();
        let mut previous = 2;
        for (i, client) in [
            "Claude", "Codex", "ZCode", "Codex", "Codex", "Claude", "Custom",
        ]
        .iter()
        .enumerate()
        {
            click(&mut ui, &format!("workflow-client-{client}"))?;
            if i == 1 {
                ui.fill_project_field("workflow-model", "模型 ID · 留空使用 CLI 默认", "gpt-test")?;
                click(&mut ui, "workflow-save-profile")?;
            }
            if i == 6 {
                ui.fill_project_field(
                    "workflow-executable",
                    "可执行文件路径 · 默认自动查找",
                    "delivery-cli",
                )?;
            }
            for retry in 0..=1 {
                if i == 2 && retry == 1 {
                    ui.fill_project_field(
                        "workflow-model",
                        "模型 ID · 留空使用 CLI 默认",
                        "team/qa-model",
                    )?;
                }
                click(&mut ui, "workflow-run")?;
                let state = read(&catalog.join("projects.json"))?;
                let id = state["projects"][0]["work"][0]["attempts"]
                    .as_array()
                    .unwrap()
                    .last()
                    .unwrap()["id"]
                    .as_str()
                    .unwrap();
                let d = attempt_dir(&catalog, project, work, id);
                if i == 6 && retry == 0 {
                    until(&mut ui, "custom CLI started", 15, |_| {
                        let ev = read(&d.join("evidence.json")).unwrap_or_default();
                        Ok(ev["workspace"]
                            .as_str()
                            .is_some_and(|cwd| Path::new(cwd).join("delivery-started").is_file())
                            .then_some(()))
                    })?;
                    click(&mut ui, &format!("workflow-stop-{id}"))?;
                }
                let ev = until(&mut ui, "stage receipt", 30, |_| {
                    let ev = read(&d.join("evidence.json")).unwrap_or_default();
                    Ok(ev["finished_at"].is_string().then_some(ev))
                })?;
                if let Some(out) = &args.output_dir {
                    let archive = out.join("game-runs").join(id);
                    fs::create_dir_all(&archive)?;
                    for entry in fs::read_dir(&d)? {
                        let entry = entry?;
                        if entry.file_type()?.is_file() {
                            fs::copy(entry.path(), archive.join(entry.file_name()))?;
                        }
                    }
                }
                if i == 6 && retry == 0 {
                    ensure!(ev["status"] == "cancelled", "{ev}");
                    continue;
                }
                if i == 2 && retry == 1 {
                    ensure!(
                        read(&d.join("zcode-model.json"))?["modelId"] == "qa-model",
                        "explicit model missing"
                    );
                }
                ensure!(
                    ev["read_memory"]
                        .as_array()
                        .context("memory receipt")?
                        .contains(&previous.into()),
                    "previous handoff missing: {ev}"
                );
                ensure!(
                    !ev["read_memory"].as_array().unwrap().contains(&1.into()),
                    "superseded memory leaked"
                );
                if i == 2 && retry == 0 {
                    ensure!(
                        ev["exit_code"] == 12 && ev["status"] == "failed",
                        "expected preflight failure: {ev}"
                    );
                    click(&mut ui, "project-next")?;
                    ensure!(
                        read(&catalog.join("projects.json"))?["projects"][0]["work"][0]["stage"]
                            == 2,
                        "failed preflight advanced"
                    );
                    ui.checkpoint("game-preflight-failure")?;
                    ui.frame(|w, _| w.remove_window())?;
                    ui = Driver::open_with_runner(&ws, args, runner.clone())?;
                    ui.click("back-to-project")?;
                    ui.click("project-work")?;
                    continue;
                }
                ensure!(ev["status"] == "succeeded", "{ev}");
                previous = ev["written_memory"].as_i64().context("handoff receipt")?;
                if i == 3 {
                    ensure!(
                        Path::new(ev["workspace"].as_str().unwrap())
                            .join("gameplay.txt")
                            .is_file()
                            && !repo.join("gameplay.txt").exists(),
                        "implementation was not isolated"
                    );
                }
                if i == 5 {
                    ui.checkpoint("game-independent-acceptance")?;
                    click(&mut ui, &format!("workflow-inspect-{id}"))?;
                    click(&mut ui, "workflow-log-prompt.txt")?;
                    ensure!(
                        ui.element("workflow-log-content")?
                            .label()
                            .is_some_and(|s| s.contains("AEP Studio")),
                        "输入上下文未显示"
                    );
                    ui.checkpoint("game-shared-prompt")?;
                    click(&mut ui, &format!("workflow-inspect-{id}"))?;
                }
                click(&mut ui, "project-next")?;
                break;
            }
        }
        let final_state = read(&catalog.join("projects.json"))?;
        ensure!(
            final_state["projects"][0]["work"][0]["stage"] == 7,
            "game workflow did not finish"
        );
        ui.click("project-overview")?;
        ui.checkpoint("game-flow-overview")?;
        ui.click("project-memory")?;
        ui.checkpoint("project-agent-memory")?;
        if let Some(out) = &args.output_dir {
            fs::write(
                out.join("game-workflow.json"),
                serde_json::to_vec_pretty(&final_state)?,
            )?;
        }
        // A separate shell registers and finishes an already-running client while Studio stays open.
        let bin = runner.binary.as_ref().unwrap();
        let cli = |args: &[&str]| -> Result<serde_json::Value> {
            let out = std::process::Command::new(bin)
                .current_dir(&repo)
                .arg("project")
                .arg("--home")
                .arg(&catalog)
                .args(args)
                .output()?;
            ensure!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            Ok(serde_json::from_slice(&out.stdout)?)
        };
        cli(&["add-work", "独立会话交接"])?;
        ui.click("project-work")?;
        let claim = cli(&[
            "claim",
            "--stage",
            "1",
            "--client",
            "zcode",
            "--session",
            "existing-zcode-workspace",
        ])?;
        let attempt = claim["attempt"].as_str().unwrap();
        until(&mut ui, "external session visible in Studio", 15, |ui| {
            Ok((!ui.absent(&format!("workflow-stop-{attempt}"))?).then_some(()))
        })?;
        fs::write(repo.join("prd.txt"), "在原会话确认恢复范围。")?;
        cli(&[
            "submit",
            "--attempt",
            attempt,
            "--session",
            "existing-zcode-workspace",
            "--summary",
            "已确认恢复范围",
            "--file",
            "prd.txt",
            "--artifact",
            "prd.txt",
        ])?;
        until(&mut ui, "external receipt visible", 15, |ui| {
            Ok((!ui.absent("workflow-run")?).then_some(()))
        })?;
        ui.checkpoint("external-session-handoff")?;
        click(&mut ui, "project-next")?;
        ensure!(
            cli(&["status"])?["stage"] == 2,
            "Studio and CLI disagree about the approved stage"
        );
        Ok(())
    })();
    if result.is_err() {
        let _ = ui.checkpoint("workflow-failure");
    }
    result
}

pub(super) fn live(args: &mut Args) -> Result<()> {
    let parent = args
        .output_dir
        .as_ref()
        .context("missing output directory")?;
    let output = tempfile::Builder::new()
        .prefix("software-")
        .tempdir_in(parent)?
        .keep()
        .canonicalize()?;
    args.output_dir = Some(output.clone());
    let repo = output.join("repository");
    fs::create_dir(&repo)?;
    let (ws, _) = fixture(&repo)?;
    let catalog = ws.data.join("studio-projects");
    let runner = aep_studio::Runner {
        binary: Some(handoff_fixture::build_runner()?),
        client_bin: None,
    };
    let mut ui = Driver::open_with_runner(&ws, args, runner)?;
    let result = (|| -> Result<()> {
        ui.click("back-to-project")?;
        ui.click("project-name")?;
        ui.input("桌面客户端交付验证")?;
        ui.click("project-create")?;
        ui.click("project-repos")?;
        ui.click("project-repo-path")?;
        ui.input(repo.to_str().unwrap())?;
        ui.click("project-add-path")?;
        ui.click("project-memory")?;
        click(&mut ui, "project-memory-text")?;
        ui.input("需求 AC-1：实现阶段新增 STUDIO_READY.txt，内容仅一行 AEP-READY。无界面改动。测试和验收须在事项工作区读取并核对该文件。其余阶段不得修改文件。")?;
        click(&mut ui, "project-memory-save")?;
        ui.click("project-overview")?;
        ui.click("project-new-work")?;
        ui.click("project-work-name")?;
        ui.input("工作区就绪标记")?;
        ui.click("project-create-work")?;
        let state = read(&catalog.join("projects.json"))?;
        let p = &state["projects"][0];
        let project = p["id"].as_str().unwrap();
        let work = p["work"][0]["id"].as_str().unwrap();
        let mut prev = 1;
        let mut receipts = vec![];
        for (i, client) in [
            "Claude", "Codex", "Codex", "Codex", "ZCode", "Claude", "Codex",
        ]
        .into_iter()
        .enumerate()
        {
            click(&mut ui, &format!("workflow-client-{client}"))?;
            let model = match client {
                "Codex" => args.codex_model.as_deref(),
                "ZCode" => args.zcode_model.as_deref(),
                _ => None,
            };
            if let Some(m) = model {
                click(&mut ui, "workflow-model")?;
                ui.replace_focused(m)?;
            }
            click(&mut ui, "project-stage-note")?;
            ui.input(
                "执行项目决策 AC-1。用简短中文说明当前阶段交付物并保留 AC-1。无需构建或发布。",
            )?;
            click(&mut ui, "workflow-run")?;
            let state = read(&catalog.join("projects.json"))?;
            let id = state["projects"][0]["work"][0]["current_attempt"][i.to_string()]
                .as_str()
                .context("missing attempt")?;
            let dir = attempt_dir(&catalog, project, work, id);
            let ev = until(&mut ui, "model CLI completion", 920, |_| {
                let e = read(&dir.join("evidence.json")).unwrap_or_default();
                Ok(e["finished_at"].is_string().then_some(e))
            })?;
            ensure!(
                ev["status"] == "succeeded",
                "{} 阶段 {} · {} · {}",
                client,
                i + 1,
                dir.display(),
                ev["error"]
            );
            ensure!(
                ev["read_memory"]
                    .as_array()
                    .context("missing memory receipt")?
                    .contains(&prev.into()),
                "交接记忆丢失"
            );
            prev = ev["written_memory"]
                .as_i64()
                .context("missing written memory")?;
            if i >= 3 {
                let tree = Path::new(ev["workspace"].as_str().unwrap());
                ensure!(
                    fs::read_to_string(tree.join("STUDIO_READY.txt"))?.trim() == "AEP-READY"
                        && !repo.join("STUDIO_READY.txt").exists(),
                    "AC-1 或仓库隔离失败"
                );
            }
            receipts.push(serde_json::json!({"stage":ev["stage"],"cli":client,"attempt":dir,"read_memory":ev["read_memory"],"written_memory":prev}));
            fs::write(
                output.join("receipts.json"),
                serde_json::to_vec_pretty(&receipts)?,
            )?;
            ui.checkpoint(&format!("software-{}-{}", i + 1, client.to_lowercase()))?;
            click(&mut ui, "project-next")?;
        }
        ensure!(
            read(&catalog.join("projects.json"))?["projects"][0]["work"][0]["stage"] == 7,
            "未归档"
        );
        ui.click("project-overview")?;
        ui.checkpoint("software-delivered")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = ui.checkpoint("software-interrupted");
    }
    println!("软件流程执行档案 {}", output.display());
    result
}
