use super::{Args, Driver, https_queue::until, project::click};
use aep::{
    project::{
        catalog_dir,
        directory::{DirectoryAction, DirectoryProject},
        save_json,
    },
    workflow::{Client, Role},
    workspace::{Workspace, ident, read},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Deserialize, Serialize)]
struct Profile {
    client: Client,
    model: String,
    brief: String,
}

#[derive(Deserialize, Serialize)]
struct Plan {
    repository: PathBuf,
    work: String,
    stage: usize,
    task: String,
    first: super::stage_relay::Models,
    second: super::stage_relay::Models,
    #[serde(default)]
    decision: String,
    test: Profile,
    accept: Profile,
    deliver: Profile,
}

// Selecting the same item in another Studio window can advance the catalog revision.
pub(super) fn open_item(ui: &mut Driver<'_>, work: &str) -> Result<()> {
    until(ui, "project", 30, |ui| {
        Ok((!ui.absent("project-overview")?).then_some(()))
    })?;
    for _ in 0..3 {
        ui.click("project-overview")?;
        click(ui, &format!("project-item-{work}"))?;
        if !ui.absent("work-timeline-tab")? {
            return Ok(());
        }
        ensure!(
            !ui.absent("project-error")?
                && ui
                    .element("project-error")?
                    .label()
                    .is_some_and(|e| e.contains("另一窗口更新")),
            "无法打开事项 {work}"
        );
    }
    anyhow::bail!("其他窗口持续更新项目库，请重试打开事项 {work}")
}

pub(super) fn wait_for_operator(
    label: &str,
    mut confirmed: impl FnMut() -> Result<bool>,
) -> Result<()> {
    println!("等待 Studio 确认 · {label}");
    while !confirmed()? {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    Ok(())
}

fn step(out: &Path, name: &str, flag: &str, plan: &Value) -> Result<PathBuf> {
    let dir = out.join(name);
    fs::create_dir_all(&dir)?;
    save_json(&dir, "input.json", plan)?;
    println!("Studio · {name} · {}", dir.display());
    let status = Command::new(std::env::current_exe()?)
        .arg(flag)
        .arg(dir.join("input.json"))
        .arg("--output-dir")
        .arg(&dir)
        .arg("--screenshots")
        .status()?;
    ensure!(status.success(), "{name} 中断，可在同一输出目录继续");
    Ok(dir)
}

pub(super) fn run(args: &Args, path: &Path) -> Result<()> {
    let mut plan: Plan = serde_json::from_slice(&fs::read(path)?)?;
    let ws = Workspace::open(&plan.repository)?;
    plan.repository = ws.root.clone();
    ident(&plan.work, 64)?;
    ident(&plan.task, 63)?;
    let out = args
        .output_dir
        .as_deref()
        .context("output directory")?
        .canonicalize()?;
    let lock = fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(out.join("run.lock"))?;
    lock.try_lock().context("该输出目录已有流程在运行")?;
    let mut catalog = DirectoryProject::resolve(
        &ws,
        &catalog_dir().context("项目目录")?,
        None,
        DirectoryAction::Restore,
    )?;
    let project = catalog
        .library
        .projects
        .iter()
        .find(|p| p.work.iter().any(|w| w.id == plan.work))
        .context("此 Studio 项目库中没有所选事项")?;
    let work = project
        .work
        .iter()
        .find(|w| w.id == plan.work)
        .context("目录绑定的项目中没有所选事项")?;
    let lock_dir = catalog
        .home
        .join("workflows")
        .join(&project.id)
        .join(&work.id);
    fs::create_dir_all(&lock_dir)?;
    let item_lock = fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_dir.join("stage-lifecycle.lock"))?;
    item_lock.try_lock().context("同一事项已有流程在运行")?;
    let start = plan.stage.checked_sub(1).context("阶段从 1 开始")?;
    let stages = work.template.stages();
    ensure!(
        stages
            .get(start..)
            .is_some_and(|s| s.iter().map(|s| s.role).eq([
                Role::Build,
                Role::Test,
                Role::Accept,
                Role::Deliver
            ])),
        "请选择测试、独立验收、交付之前的开发实现阶段"
    );
    ensure!(
        (start..stages.len()).all(|i| work.target(i).as_deref() == Some(ws.root.as_path())),
        "各阶段必须使用任务仓库以验证同一提交"
    );
    for p in [&plan.test, &plan.accept, &plan.deliver] {
        ensure!(
            !p.model.trim().is_empty() && !p.brief.trim().is_empty(),
            "每阶段需要模型及说明"
        );
    }
    for m in [&plan.first, &plan.second] {
        m.validate()?;
    }
    ensure!(
        !args.rehearse || !plan.decision.trim().is_empty(),
        "演练需指定修订正文 decision"
    );
    let config = json!({"project":project.id,"plan":plan,"rehearse":args.rehearse});
    if out.join("plan.json").exists() {
        ensure!(
            read(&out.join("plan.json"))? == config,
            "此目录已绑定另一组事项、模型或确认方式"
        );
    } else {
        ensure!(
            fs::read_dir(&out)?.all(|e| e.is_ok_and(|e| e.file_name() == "run.lock")),
            "新流程需要空输出目录"
        );
        ensure!(work.stage == start, "新流程必须从当前开发实现阶段开始");
        save_json(&out, "plan.json", &config)?;
    }
    if out.join("deliver/archive.json").try_exists()? {
        return audit(&out, &plan);
    }
    if catalog.library.active.as_ref() != Some(&project.id) {
        let pid = project.id.clone();
        catalog.library.change(&catalog.home, |library| {
            library.active = Some(pid);
            Ok(())
        })?;
    }
    let mut relay = json!(plan);
    relay["operator_gates"] = json!(!args.rehearse);
    let relay_dir = step(&out, "relay", "--live-stage-relay", &relay)?;
    let mut source = read(&relay_dir.join("second-run.json"))?["run_id"]
        .as_str()
        .context("第二轮执行")?
        .to_owned();
    for (i, (name, p)) in [
        ("test", &plan.test),
        ("accept", &plan.accept),
        ("deliver", &plan.deliver),
    ]
    .into_iter()
    .enumerate()
    {
        let next = json!({"repository":ws.root,"work":plan.work,"stage":plan.stage+i,"source_run":source,
            "client":p.client,"model":p.model,"brief":p.brief,"finalize_delivery":name=="deliver","operator_gates":!args.rehearse});
        let dir = step(&out, name, "--live-stage-transfer", &next)?;
        let previous = source.clone();
        source = read(&dir.join("destination.json"))?["run_id"]
            .as_str()
            .context("目标执行")?
            .to_owned();
        if name == "accept" {
            let path = aep::workflow::attempt_dir(
                &catalog.home,
                config["project"].as_str().context("项目 ID")?,
                &plan.work,
                &source,
            );
            acceptance_cites(&path, &previous)?;
        }
    }
    audit(&out, &plan)
}

fn acceptance_cites(dir: &Path, test_run: &str) -> Result<()> {
    ensure!(
        fs::read_to_string(dir.join("result.txt"))?.contains(test_run),
        "独立验收报告缺少原始测试执行 ID {test_run}"
    );
    Ok(())
}

fn evidence_path(root: &Path, name: &Path) -> Result<PathBuf> {
    ensure!(
        !name.as_os_str().is_empty()
            && name
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
        "归档路径无效 {}",
        name.display()
    );
    let path = root.join(name);
    ensure!(
        path.canonicalize()?.starts_with(root.canonicalize()?),
        "证据不在归档目录内"
    );
    Ok(path)
}

/// Publish the portable copy only after every indexed file has been copied.
fn export_bundle(root: &Path, out: &Path, manifest: &Value) -> Result<usize> {
    let files = manifest["files"].as_object().context("证据文件清单")?;
    let target = out.join("evidence");
    let temp = if target.try_exists()? {
        None
    } else {
        Some(tempfile::tempdir_in(out)?)
    };
    for name in files
        .keys()
        .map(String::as_str)
        .chain(["manifest.json", "timeline.json"])
    {
        let original = evidence_path(root, Path::new(name))?;
        if let Some(size) = files.get(name) {
            ensure!(
                size.as_u64() == Some(original.metadata()?.len()),
                "证据大小不符 {name}"
            );
        }
        if let Some(dir) = &temp {
            let copy = dir.path().join(name);
            fs::create_dir_all(copy.parent().context("证据父目录")?)?;
            fs::copy(original, copy)?;
        } else {
            ensure!(
                fs::read(original)? == fs::read(evidence_path(&target, Path::new(name))?)?,
                "导出证据不一致 {name}"
            );
        }
    }
    if let Some(dir) = temp {
        fs::rename(dir.path(), target)?;
    }
    Ok(files.len())
}

fn audit(out: &Path, plan: &Plan) -> Result<()> {
    let result = verify_delivery(out, plan);
    if let Err(error) = &result {
        save_json(
            out,
            "verified.json",
            &json!({"verified":false,"work":plan.work,"error":format!("{error:#}")}),
        )?;
    }
    result
}

fn verify_delivery(out: &Path, plan: &Plan) -> Result<()> {
    let archive = read(&out.join("deliver/archive.json"))?;
    let root = Path::new(archive["directory"].as_str().context("归档目录")?);
    let manifest = read(&root.join("manifest.json"))?;
    let rows = manifest["runs"].as_array().context("归档执行")?;
    let receipt = &archive["receipt"];
    ensure!(
        manifest["delivery"] == *receipt
            && receipt["work"] == plan.work
            && receipt["project"] == read(&out.join("plan.json"))?["project"],
        "错误事项的归档"
    );
    let mut ids = vec![];
    let mut dirs = vec![];
    for local in [
        "relay/first-run.json",
        "relay/second-run.json",
        "test/destination.json",
        "accept/destination.json",
        "deliver/destination.json",
    ] {
        let ev = read(&out.join(local))?;
        let id = ev["run_id"].as_str().context("run_id")?;
        let row = rows
            .iter()
            .find(|r| r["run"] == id)
            .context("归档缺少执行")?;
        let dir = evidence_path(
            root,
            Path::new(row["directory"].as_str().context("执行目录")?),
        )?;
        ensure!(
            read(&dir.join("evidence.json"))? == ev,
            "归档回执与实际执行不符 {id}"
        );
        ids.push(id.to_owned());
        dirs.push(dir);
    }
    let first = read(&dirs[0].join("evidence.json"))?;
    let second = read(&dirs[1].join("evidence.json"))?;
    let decision = second["request"]["require_memory"]
        .as_i64()
        .context("修订决策")?;
    ensure!(
        second["request"]["handoff_from_run"] == ids[0]
            && first["candidate_sha"].is_string()
            && second["requested_candidate_sha"] == first["candidate_sha"],
        "第二轮交接来源错误"
    );
    let snapshot = read(&root.join("timeline.json"))?;
    ensure!(
        snapshot["work_id"] == plan.work
            && snapshot["deliveries"]
                .as_array()
                .is_some_and(|a| a.contains(receipt)),
        "时间线未关联交付"
    );
    acceptance_cites(&dirs[3], &ids[2])?;
    let revision = relay_revision(plan, &snapshot, &ids, &dirs, decision)?;
    let mut stages = vec![];
    for (i, name) in ["test", "accept", "deliver"].into_iter().enumerate() {
        let ev = read(&dirs[i + 2].join("evidence.json"))?;
        let confirmed = read(&out.join(name).join("confirmed.json"))?;
        ensure!(
            ev["status"] == "succeeded"
                && ev["read_stage_transfer"] == confirmed
                && confirmed["run"] == ids[i + 1]
                && confirmed["work"] == plan.work
                && confirmed["decision_id"] == decision
                && ev["read_repository_memory"]
                    .as_array()
                    .is_some_and(|a| a.contains(&json!(decision))),
            "{name} 未读取已确认的同一决策"
        );
        let profile = [&plan.test, &plan.accept, &plan.deliver][i];
        ensure!(
            ev["profile"]["client"] == json!(profile.client)
                && ev["profile"]["model"] == profile.model
                && read(&dirs[i + 2].join("request.json"))?["brief"] == profile.brief
                && confirmed["from"] == plan.stage - 1 + i
                && confirmed["to"] == plan.stage + i
                && snapshot["stage_transfers"]
                    .as_array()
                    .is_some_and(|a| a.contains(&confirmed)),
            "阶段配置或交接不匹配 {name}"
        );
        if i == 0 {
            ensure!(
                second["candidate_sha"].is_string()
                    && confirmed["candidate"] == second["candidate_sha"],
                "测试未接收第二轮提交"
            );
        }
        stages.push(json!({"stage":name,"run":ids[i+2],"profile":ev["profile"],"session":ev["session"],"reported_models":ev["reported_models"],"source":ids[i+1]}));
        if i > 0 {
            ensure!(
                ev["source_evidence"]["run"] == ids[i + 1]
                    && fs::read(dirs[i + 2].join("inputs/source-evidence.json"))?
                        == fs::read(dirs[i + 1].join("evidence.json"))?,
                "{name} 未读取原始回执"
            );
        }
        if i < 2 {
            ensure!(
                fs::read_to_string(dirs[i + 2].join("result.txt"))?
                    .trim()
                    .lines()
                    .last()
                    .is_some_and(|l| l.trim() == "AEP: PASS"),
                "{name} 未通过"
            );
        }
    }
    ensure!(
        receipt["accepted_run"] == ids[3] && receipt["run"] == ids[4],
        "交付归档与独立验收不匹配"
    );
    let screenshots = screenshots(out)?;
    let count = export_bundle(root, out, &manifest)?;
    save_json(
        out,
        "verified.json",
        &json!({"verified":true,"work":plan.work,"decision":decision,"revision":revision,
        "runs":ids,"candidate":second["candidate_sha"],"stage_receipts":stages,"studio_archive":root,"evidence":"evidence","screenshots":screenshots,"checked_artifacts":count}),
    )?;
    println!("验证通过 · {}", out.join("verified.json").display());
    Ok(())
}

fn screenshots(out: &Path) -> Result<Vec<String>> {
    let mut screenshots = Vec::new();
    for (name, checkpoints) in [
        (
            "relay",
            vec![
                "stage-first",
                "stage-second",
                "stage-timeline-round-2",
                "stage-run-evidence",
            ],
        ),
        ("test", vec!["reopened-source", "reopened-destination"]),
        ("accept", vec!["reopened-source", "reopened-destination"]),
        (
            "deliver",
            vec![
                "reopened-source",
                "reopened-destination",
                "reopened-delivery-archive",
            ],
        ),
    ] {
        for checkpoint in checkpoints {
            let path = format!("{name}/{checkpoint}.png");
            ensure!(out.join(&path).is_file(), "缺少截图 {path}");
            screenshots.push(path);
        }
    }
    // Index additional UI checkpoints (for example the decision editor or confirmation dialog).
    for phase in ["relay", "test", "accept", "deliver"] {
        for entry in fs::read_dir(out.join(phase))? {
            let path = entry?.path();
            if path.extension().is_some_and(|s| s == "png")
                && path.file_stem().is_some_and(|s| s != "failure")
            {
                let png = fs::read(&path)?;
                ensure!(
                    png.starts_with(b"\x89PNG\r\n\x1a\n")
                        && png.ends_with(b"IEND\xaeB`\x82")
                        && read(&path.with_extension("json"))?.is_array(),
                    "截图或控件快照无效 {}",
                    path.display()
                );
                let relative = path.strip_prefix(out)?.to_string_lossy().into_owned();
                if !screenshots.contains(&relative) {
                    screenshots.push(relative);
                }
            }
        }
    }
    screenshots.sort();
    Ok(screenshots)
}

fn relay_revision<'a>(
    plan: &Plan,
    snapshot: &'a Value,
    ids: &[String],
    dirs: &[PathBuf],
    decision: i64,
) -> Result<&'a Value> {
    let timeline = snapshot["rounds"].as_array().context("时间线执行")?;
    let mut revision = None;
    for (i, models) in [&plan.first, &plan.second].into_iter().enumerate() {
        let row = timeline
            .iter()
            .find(|r| r["run"] == ids[i])
            .context("缺少开发记录")?;
        let ev = read(&dirs[i].join("evidence.json"))?;
        models.verify_receipt(&ev)?;
        ensure!(
            row["round"] == i + 1
                && row["stage_index"] == plan.stage - 1
                && ev["task"] == plan.task,
            "接力事项或轮次不符"
        );
        if i == 1 {
            for role in ["implementer", "reviewer"] {
                ensure!(
                    ev["memory_inputs"][role]
                        .as_array()
                        .is_some_and(|a| a.contains(&json!(decision))),
                    "{role} 未读取第二轮决策"
                );
            }
            let first = timeline
                .iter()
                .find(|r| r["run"] == ids[0])
                .context("第一轮记录")?;
            let agents = row["agents"].as_array().context("第二轮客户端")?;
            ensure!(agents.len() == 2, "缺少实现或审查端");
            for agent in agents {
                let note = agent["reads"]
                    .as_array()
                    .context("读取记录")?
                    .iter()
                    .find(|n| n["id"] == decision && n["kind"] == "decision")
                    .context("缺少决策正文")?;
                ensure!(
                    first["agents"]
                        .as_array()
                        .context("第一轮客户端")?
                        .iter()
                        .filter_map(|a| a["writes"].as_array())
                        .flatten()
                        .any(|q| q["id"] == note["replaces"] && q["kind"] == "open_question"),
                    "决策来源并非第一轮问题"
                );
                ensure!(revision.is_none_or(|r| r == note), "两个角色读到的决策不同");
                revision = Some(note);
            }
        }
    }
    let revision = revision.context("决策修订")?;
    Ok(revision)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn an_incomplete_or_changed_export_does_not_verify() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let source = tmp.path().join("archive");
        let out = tmp.path().join("output");
        fs::create_dir_all(&source)?;
        fs::create_dir(&out)?;
        let manifest = json!({"files":{"result.txt":4}});
        save_json(&source, "manifest.json", &manifest)?;
        save_json(&source, "timeline.json", &json!({}))?;
        assert!(export_bundle(&source, &out, &manifest).is_err());
        assert!(!out.join("evidence").exists());
        fs::write(source.join("result.txt"), "PASS")?;
        assert_eq!(export_bundle(&source, &out, &manifest)?, 1);
        assert_eq!(export_bundle(&source, &out, &manifest)?, 1);
        fs::write(out.join("evidence/result.txt"), "FAIL")?;
        assert!(export_bundle(&source, &out, &manifest).is_err());
        assert!(evidence_path(&source, Path::new("../archive/result.txt")).is_err());
        Ok(())
    }
}
