use super::{Args, Driver, fixture, handoff_fixture, https_queue::until};
use aep::workspace::Workspace;
use aep::workspace::read;
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

pub(super) fn click(ui: &mut Driver<'_>, id: &str) -> Result<()> {
    if !ui.element(id)?.visible() {
        ui.reveal("project-page", id)?;
    }
    ui.click(id)
}
fn archived_records(ws: &Workspace, operator: &Path) -> Result<()> {
    let https = ws.data.join("https-studio/studio-existing");
    let relay = ws.data.join("relays/relay-existing");
    fs::create_dir_all(&https)?;
    fs::create_dir_all(&relay)?;
    let connection =
        json!({"endpoint":"https://127.0.0.1:9123","operator_file":operator,"ca":null});
    fs::write(
        https.parent().unwrap().join("queue-site.json"),
        serde_json::to_vec(&connection)?,
    )?;
    let mut request = connection;
    request["ticket"] = json!("https-task-existing");
    request["task"] = json!({"name":"resume-task","sha":"0123456789abcdef"});
    request["branch"] = json!({"name":"test/archive","sha":"0123456789abcdef"});
    fs::write(https.join("request.json"), serde_json::to_vec(&request)?)?;
    fs::write(
        https.join("state.json"),
        br#"{"stage":"remote_outage","reason":"fixture outage","exit_code":4}"#,
    )?;
    fs::write(relay.join("relay.json"),br#"{"id":"relay-existing","task":{"name":"resume-task","sha":"0123456789abcdef"},"model":"archive-model","attempts":[]}"#)?;
    fs::write(relay.join("owner.lock"), "")?;
    Ok(())
}
fn project_cli(binary: &Path, repo: &Path, home: &Path, args: &[&str]) -> Result<Value> {
    let r = Command::new(binary)
        .current_dir(repo)
        .args(["project", "--home"])
        .arg(home)
        .args(args)
        .output()?;
    ensure!(
        r.status.success(),
        "project CLI: {}",
        String::from_utf8_lossy(&r.stderr)
    );
    Ok(serde_json::from_slice(&r.stdout)?)
}
pub(super) fn run(args: &Args) -> Result<()> {
    let root = tempfile::tempdir()?;
    let client = root.path().join("studio");
    let worker = root.path().join("worker");
    fs::create_dir(&client)?;
    fs::create_dir(&worker)?;
    let (ws, _) = fixture(&client)?;
    fixture(&worker)?;
    let binary = handoff_fixture::build_runner()?;
    let runner = aep_studio::Runner {
        binary: Some(binary.clone()),
        client_bin: None,
    };
    let data = ws.data.join("studio-projects/projects.json");
    let mut ui = Driver::open_with_runner(&ws, args, runner.clone())?;
    let result = (|| -> Result<()> {
        ui.click("back-to-project")?;
        ui.click("project-name")?;
        ui.input("交付平台")?;
        ui.click("project-create")?;
        ui.click("project-repos")?;
        for path in [&client, &worker] {
            ui.click("project-repo-path")?;
            ui.input(path.to_str().unwrap())?;
            ui.click("project-add-path")?;
        }
        ui.checkpoint("project-repositories")?;
        ui.click("project-overview")?;
        ui.click("project-new-work")?;
        ui.click("project-work-name")?;
        ui.input("任务失败后恢复")?;
        ui.click("project-create-work")?;
        click(&mut ui, "project-stage-note")?;
        ui.input("关闭 Studio 后能恢复上一次执行记录。")?;
        click(&mut ui, "project-next")?;
        click(&mut ui, "project-stage-note")?;
        ui.input("Studio 保存进度，Worker 保留执行回执。")?;
        click(&mut ui, "project-next")?;
        click(&mut ui, "project-stage-note")?;
        ui.input("技术方案已确认。")?;
        click(&mut ui, "project-next")?;
        let catalog = read(&data)?;
        let feature = &catalog["projects"][0]["work"][0];
        ensure!(
            feature["stage"] == 3 && feature["repositories"].as_array().unwrap().len() == 2,
            "workflow did not retain both repositories and its stage"
        );
        let key = worker.canonicalize()?;
        let ticket = feature["tickets"][key.to_str().unwrap()]
            .as_str()
            .unwrap()
            .to_owned();
        // Only the worker has delivery tickets. Its progress must stay scoped to that repository.
        let base = aep::workspace::commit(&worker, "HEAD")?;
        let branch = aep::workspace::git(&worker, &["branch", "--show-current"])?;
        let draft = root.path().join("task.json");
        fs::write(
            &draft,
            serde_json::to_vec(
                &serde_json::json!({"name":"resume-task","spec":"Preserve the execution receipt.","manifest":{"title":"恢复回执","allowed_paths":["receipt.rs"],"check":["true"]}}),
            )?,
        )?;
        for ticket in [ticket.as_str(), "existing-worker-task"] {
            let status = Command::new(&binary)
                .current_dir(&worker)
                .args([
                    "local",
                    "author",
                    "--ticket",
                    ticket,
                    "--target",
                    branch.trim(),
                    "--baseline",
                    &base,
                    "--draft",
                ])
                .arg(&draft)
                .output()?;
            ensure!(
                status.status.success(),
                "fixture author: {}",
                String::from_utf8_lossy(&status.stderr)
            );
        }
        click(&mut ui, "project-refresh-work")?;
        click(&mut ui, "work-task-1")?;
        until(&mut ui, "scoped ticket", 25, |ui| {
            Ok((!ui.absent("local-approve-plan")?).then_some(()))
        })?;
        ui.click("project-toggle-catalog")?;
        ui.click("local-ticket-existing-worker-task")?;
        until(&mut ui, "bind existing task", 25, |ui| {
            Ok((!ui.absent("project-bind-ticket")?).then_some(()))
        })?;
        ensure!(
            ui.absent("local-approve-plan")?,
            "unlinked ticket offered approval"
        );
        ui.click("project-bind-ticket")?;
        ensure!(
            read(&data)?["projects"][0]["work"][0]["tickets"][key.to_str().unwrap()]
                == "existing-worker-task",
            "existing ticket was not linked to the work"
        );
        ui.click("back-to-project")?;
        ui.checkpoint("project-development")?;
        click(&mut ui, "project-stage-1")?;
        click(&mut ui, "work-context-0")?;
        ensure!(
            !ui.absent("new-note")?,
            "design memory did not open in its repository"
        );
        ui.click("back-to-project")?;
        ui.click("project-overview")?;
        ui.checkpoint("project-overview")?;
        ui.click("project-filter-0")?;
        let feature_button = format!("project-item-{}", feature["id"].as_str().unwrap());
        ensure!(
            ui.absent(&feature_button)?,
            "stage filter included an unrelated item"
        );
        ui.click("project-filter-3")?;
        ensure!(
            !ui.absent(&feature_button)?,
            "stage filter lost the active item"
        );
        ui.click("project-clear-filter")?;
        ui.click("project-library")?;
        ui.click("project-name")?;
        ui.input("第二个项目")?;
        ui.click("project-create")?;
        ui.click("project-new-work")?;
        ui.click("project-kind-版本")?;
        ui.click("project-work-name")?;
        ui.input("v1.0")?;
        ui.click("project-create-work")?;
        for label in [
            "范围确认",
            "交互确认",
            "方案确认",
            "开发完成",
            "测试通过",
            "验收通过",
            "发布归档",
        ] {
            click(&mut ui, "project-stage-note")?;
            ui.input(label)?;
            click(&mut ui, "project-next")?;
        }
        let second = read(&data)?;
        ensure!(
            second["projects"][1]["work"][0]["stage"] == 7
                && second["projects"][0]["work"][0]["stage"] == 3,
            "project state leaked"
        );
        ui.frame(|w, _| w.remove_window())?;
        ui = Driver::open_with_runner(&ws, args, runner.clone())?;
        ui.click("back-to-project")?;
        let home = data.parent().unwrap();
        let id = catalog["active"].as_str().unwrap();
        project_cli(&binary, &client, home, &["--project", id, "open"])?;
        ensure!(
            !ui.absent("project-open-directory")?,
            "missing folder picker"
        );
        ui.click("project-sync")?;
        ensure!(
            !ui.absent("project-sync-result")?,
            "manual sync did not report completion"
        );
        ui.checkpoint("project-manual-sync")?;
        ui.click("project-work")?;
        click(&mut ui, "project-stage-1")?;
        ensure!(
            ui.by_label("阶段记录")?.value() == Some("Studio 保存进度，Worker 保留执行回执。"),
            "design record lost after restart"
        );
        let worker_ws = Workspace::open(&worker)?;
        let operator = root.path().join("operator");
        fs::write(&operator, "fixture-operator-file-only")?;
        archived_records(&worker_ws, &operator)?;
        ui.click("project-settings")?;
        click(&mut ui, "settings-edit-1")?;
        ensure!(
            ui.by_label("HTTPS 控制面")?.value() == Some("https://127.0.0.1:9123"),
            "legacy site missing"
        );
        click(&mut ui, "site-endpoint")?;
        ui.replace_focused("https://127.0.0.1:9135")?;
        click(&mut ui, "settings-save")?;
        ensure!(
            project_cli(&binary, &worker, home, &["site", "--repository", "."])?["endpoint"]
                == "https://127.0.0.1:9135",
            "project site not saved"
        );
        ensure!(
            read(&worker_ws.data.join("https-studio/queue-site.json"))?["endpoint"]
                == "https://127.0.0.1:9123",
            "legacy connection overwritten"
        );
        ui.checkpoint("project-settings")?;
        click(&mut ui, "settings-workers-1")?;
        ensure!(
            !ui.absent("https-pilot-page")?
                && ui.element("https-queue-endpoint")?.label() == Some("https://127.0.0.1:9135")
                && ui.absent("https-start")?,
            "missing HTTPS infrastructure"
        );
        ui.click("back-to-project")?;
        ensure!(!ui.absent("settings-edit-1")?, "wrong return destination");
        click(&mut ui, "settings-rotation-1")?;
        ensure!(
            !ui.absent("https-rolling-mode")? && ui.absent("https-start")?,
            "wrong rotation destination"
        );
        ui.click("back-to-project")?;
        ui.click("project-work")?;
        ui.click("project-new-work")?;
        ui.click("project-work-name")?;
        ui.input("发布前验收")?;
        ui.click("project-create-work")?;
        let receiver = read(&data)?["projects"][0]["selected"]
            .as_str()
            .unwrap()
            .to_string();
        click(&mut ui, &feature_button)?;
        click(&mut ui, "work-collab-tab")?;
        click(&mut ui, &format!("handoff-to-{receiver}"))?;
        click(&mut ui, "work-handoff-note")?;
        ui.input("沿用原票据重试，验收见 receipt.rs。")?;
        click(&mut ui, "work-send-handoff")?;
        click(&mut ui, "work-https-1")?;
        ensure!(
            ui.absent("https-stage")?
                && ui.absent("https-start")?
                && ui.absent("https-rolling-mode")?,
            "unrelated session or infrastructure exposed"
        );
        ui.click("pick-https-record")?;
        ui.click("link-studio-existing")?;
        ensure!(
            !ui.absent("https-stage")? && !ui.absent("https-resume")?,
            "linked HTTPS session missing"
        );
        ui.checkpoint("work-https-history")?;
        ui.click("back-to-project")?;
        click(&mut ui, "work-relay-1")?;
        until(&mut ui, "relay catalog", 20, |ui| {
            Ok((!ui.absent("pick-relay-record")?).then_some(()))
        })?;
        ui.click("pick-relay-record")?;
        until(&mut ui, "existing relay", 20, |ui| {
            Ok((!ui.absent("link-relay-existing")?).then_some(()))
        })?;
        ui.click("link-relay-existing")?;
        ensure!(
            ui.by_label("Codex 模型")?.value() == Some("archive-model"),
            "linked record lost its configured model"
        );
        ensure!(!ui.absent("relay-existing")?, "linked relay missing");
        ui.click("back-to-project")?;
        click(&mut ui, &format!("project-item-{receiver}"))?;
        click(&mut ui, "work-collab-tab")?;
        ui.checkpoint("work-handoff-inbox")?;
        let packet = project_cli(&binary, &worker, home, &["--work", &receiver, "context"])?;
        ensure!(
            packet["context"]
                .as_str()
                .unwrap()
                .contains("沿用原票据重试"),
            "handoff missing from CLI"
        );
        click(&mut ui, "work-https-1")?;
        ensure!(
            ui.absent("https-session-studio-existing")? && ui.absent("https-resume")?,
            "other item's HTTPS session leaked"
        );
        ui.click("back-to-project")?;
        click(&mut ui, "work-relay-1")?;
        ensure!(ui.absent("relay-existing")?, "other item's relay leaked");
        let saved = project_cli(
            &binary,
            &worker,
            home,
            &["--work", feature["id"].as_str().unwrap(), "status"],
        )?;
        ensure!(
            saved["records"].as_array().unwrap().len() == 2,
            "associations missing"
        );
        ui.frame(|w, _| w.remove_window())?;
        ui = Driver::open_with_runner(&ws, args, runner)?;
        ui.click("back-to-project")?;
        ui.click("project-work")?;
        click(&mut ui, &feature_button)?;
        click(&mut ui, "work-collab-tab")?;
        click(&mut ui, "work-https-1")?;
        ensure!(!ui.absent("https-resume")?, "linked session not restored");
        ui.click("back-to-project")?;
        // A mistaken link can be removed without touching the original evidence.
        click(&mut ui, "unlink-https-0")?;
        ensure!(
            worker_ws
                .data
                .join("https-studio/studio-existing/request.json")
                .is_file(),
            "unlink deleted evidence"
        );
        if let Some(dir) = &args.output_dir {
            fs::write(
                dir.join("project-state.json"),
                serde_json::to_vec_pretty(&read(&data)?)?,
            )?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = ui.checkpoint("project-failure");
    }
    result
}
