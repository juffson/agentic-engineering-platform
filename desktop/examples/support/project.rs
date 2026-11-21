use super::{Args, Driver, fixture, handoff_fixture, https_queue::until};
use aep::workspace::read;
use anyhow::{Result, ensure};
use std::{fs, process::Command};

pub(super) fn click(ui: &mut Driver<'_>, id: &str) -> Result<()> {
    if !ui.element(id)?.visible() {
        ui.reveal("project-page", id)?;
    }
    ui.click(id)
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
        ui = Driver::open_with_runner(&ws, args, runner)?;
        ui.click("back-to-project")?;
        ui.click("project-library")?;
        let id = catalog["active"].as_str().unwrap();
        ui.click(&format!("project-open-{id}"))?;
        ui.click("project-work")?;
        click(&mut ui, "project-stage-1")?;
        ensure!(
            ui.by_label("阶段记录")?.value() == Some("Studio 保存进度，Worker 保留执行回执。"),
            "design record lost after restart"
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
