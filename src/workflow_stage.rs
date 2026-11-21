use crate::{agent::logged, run::save};
use aep::{
    cancel::Cancellation,
    memory::{Key, Memory, TEXT_LIMIT},
    workflow::{Client, Request, Role, Stage, tail},
    workspace::{Workspace, git},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

fn workspace(work: &Path, repository: Option<&Path>) -> Result<PathBuf> {
    let Some(repo) = repository else {
        let p = work.join("workspace");
        fs::create_dir_all(&p)?;
        return Ok(p);
    };
    let repo = Workspace::open(repo)?;
    let id = work
        .file_name()
        .and_then(|s| s.to_str())
        .context("missing work ID")?;
    aep::workspace::ident(id, 64)?;
    let path = repo.data.join("workflow-worktrees").join(id);
    let branch = format!("aep-stage/{id}");
    if !path.exists() {
        fs::create_dir_all(path.parent().unwrap())?;
        git(
            &repo.root,
            &[
                "worktree",
                "add",
                "-b",
                &branch,
                path.to_str().context("invalid workspace path")?,
                "HEAD",
            ],
        )?;
    }
    ensure!(
        git(&path, &["branch", "--show-current"])?.trim() == branch,
        "事项工作区的分支已经改变，请核对后再执行"
    );
    Ok(path)
}

fn command_receipts(stdout: &Path) -> Result<Vec<Value>> {
    let mut commands = std::collections::VecDeque::new();
    for line in BufReader::new(fs::File::open(stdout)?).lines() {
        let line = line?;
        if let Ok(event) = serde_json::from_str::<Value>(&line)
            && event["type"] == "item.completed"
        {
            let item = &event["item"];
            if item["type"] == "command_execution"
                && let (Some(cmd), Some(code)) =
                    (item["command"].as_str(), item["exit_code"].as_i64())
            {
                if commands.len() == 12 {
                    commands.pop_front();
                }
                commands.push_back(
                    json!({"command":cmd.chars().take(512).collect::<String>(), "exit_code":code}),
                );
            }
        }
    }
    Ok(commands.into())
}
fn perform(
    req: &Request,
    stage: &Stage,
    dir: &Path,
    work: &Path,
    id: &str,
    stop: &Cancellation,
    record: &mut Value,
) -> Result<()> {
    req.profile.validate()?;
    let cwd = workspace(work, req.repository.as_deref())?;
    let mut memory = Memory::open(work.parent().context("missing project directory")?)?;
    let key = Key::new("", "", "");
    let notes = memory.context(&key, None)?;
    let repo_notes = if let Some(repo) = &req.repository {
        let repo = crate::git::Repo::open(repo)?;
        if repo.data.join("memory.sqlite3").is_file() {
            let scope = req
                .ticket
                .as_ref()
                .map(|id| {
                    crate::local::workflow_memory_scope(
                        &repo,
                        id,
                        &aep::workspace::commit(&cwd, "HEAD")?,
                    )
                })
                .transpose()?
                .flatten();
            Memory::open(&repo.data)?.context(scope.as_ref().unwrap_or(&key), None)?
        } else {
            vec![]
        }
    } else {
        vec![]
    };
    let gate = matches!(stage.role, Role::Accept | Role::Test);
    let write = stage.role == Role::Build;
    let mut prompt = format!(
        "AEP Studio 项目：{}\n开发事项：{}\n流程：{} · {}\n阶段职责：{}\n本次说明：{}\n人工已确认的本事项交付物（按阶段顺序）：{}\n项目共享记忆：{}\n当前仓库及关联票据的适用记忆：{}\n工作目录：{}\n{} 不要推送、合并、发布或创建 PR。直接返回本阶段的交付物、证据和给下一位 Agent 的交接。{}",
        req.project,
        req.work,
        req.template.label(),
        stage.name,
        stage.brief,
        req.brief,
        serde_json::to_string(&req.approved)?,
        serde_json::to_string(&notes)?,
        serde_json::to_string(&repo_notes)?,
        cwd.display(),
        if write {
            "仅在当前事项工作区实施改动。"
        } else {
            "本阶段保留已有实现，仅检查或验证，在回复中记录结果。"
        },
        if gate {
            "最后单独一行写 AEP: PASS 或 AEP: BLOCKED。未验证必要标准时必须 BLOCKED。"
        } else {
            ""
        }
    );
    if !req.profile.check_command.is_empty() {
        prompt.push_str(&format!(
            "\n本阶段已允许精确执行以下验收命令，请原样单独运行并报告结果：{}",
            req.profile.check_command
        ));
    }
    fs::write(dir.join("prompt.txt"), &prompt)?;
    record["workspace"] = json!(cwd);
    record["read_memory"] = json!(notes.iter().map(|v| &v["id"]).collect::<Vec<_>>());
    record["read_repository_memory"] =
        json!(repo_notes.iter().map(|v| &v["id"]).collect::<Vec<_>>());
    save(dir, record)?;
    let profile = &req.profile;
    let bin = profile.program(&std::env::var_os("PATH").unwrap_or_default())?;
    let mut args = vec![bin.display().to_string()];
    if bin == Path::new(aep::workflow::ZCODE_APP)
        && std::env::var_os("ZCODE_BUILTIN_PROVIDER_CONFIG_FILE").is_none()
    {
        args.insert(0,"ZCODE_BUILTIN_PROVIDER_CONFIG_FILE=/Applications/ZCode.app/Contents/Resources/config/provider/zcode-builtin.json".into());
        args.insert(0, "/usr/bin/env".into());
    }
    let result_path = dir.join("result.txt");
    match profile.client {
        Client::Codex => {
            args.extend(
                [
                    "exec",
                    "--ephemeral",
                    "--skip-git-repo-check",
                    "--sandbox",
                    if write || gate {
                        "workspace-write"
                    } else {
                        "read-only"
                    },
                    "--json",
                    "--output-last-message",
                ]
                .map(String::from),
            );
            args.push(result_path.display().to_string());
        }
        Client::Claude => {
            args.extend(
                [
                    "-p",
                    "--no-session-persistence",
                    "--output-format",
                    "json",
                    "--permission-mode",
                    if write { "acceptEdits" } else { "default" },
                    "--tools",
                    if write {
                        "Read,Grep,Glob,Edit,Write,Bash"
                    } else {
                        "Read,Grep,Glob,Bash"
                    },
                ]
                .map(String::from),
            );
        }
        Client::ZCode if !profile.model.is_empty() => {
            args = vec![
                std::env::current_exe()?.display().to_string(),
                "zcode-stage".into(),
                "--binary".into(),
                bin.display().to_string(),
                "--request".into(),
                dir.join("request.json").display().to_string(),
            ];
        }
        Client::ZCode => args.extend(
            [
                "--mode",
                if write {
                    "edit"
                } else if gate {
                    "guarded"
                } else {
                    "plan"
                },
                "--no-color",
                "--prompt",
                "完成附件中的 AEP 阶段任务。",
                "--attach",
            ]
            .map(String::from),
        ),
        Client::Custom => {}
    }
    if !profile.check_command.is_empty() {
        args.extend([
            "--allowedTools".to_string(),
            format!("Bash({})", profile.check_command),
        ]);
    }
    if profile.client != Client::ZCode && !profile.model.trim().is_empty() {
        args.extend(["--model".into(), profile.model.trim().into()]);
    }
    if profile.client == Client::Codex {
        args.push("-".into());
    }
    if profile.client == Client::ZCode && profile.model.is_empty() {
        args.push(dir.join("prompt.txt").display().to_string());
    }
    let stdout = dir.join("stdout.log");
    let stderr = dir.join("stderr.log");
    let code = logged(
        &args,
        &cwd,
        &stdout,
        900,
        (profile.client != Client::ZCode).then_some(prompt.as_str()),
        Some(stderr.clone()),
        stop,
    )?;
    record["exit_code"] = json!(code);
    if profile.client == Client::Codex {
        record["commands"] = json!(command_receipts(&stdout)?);
    }

    ensure!(
        code == 0,
        "{} 退出码 {}\n{}",
        profile.client.label(),
        code,
        if stderr.metadata()?.len() > 0 {
            tail(&stderr, 2400)
        } else {
            tail(&stdout, 2400)
        }
    );
    let result = match profile.client {
        Client::Codex => fs::read_to_string(&result_path).context("Codex 未生成最终答复")?,
        Client::Claude => {
            let v: Value = serde_json::from_reader(fs::File::open(stdout)?)?;
            if let Some(usage) = v["modelUsage"].as_object() {
                record["reported_models"] = json!(usage.keys().collect::<Vec<_>>());
            }
            let s = v["result"].as_str().context("Claude 未生成最终答复")?;
            ensure!(v["is_error"] != true, "Claude: {s}");
            s.into()
        }
        _ => fs::read_to_string(stdout)?,
    };
    ensure!(!result.trim().is_empty(), "CLI 返回空结果");
    fs::write(&result_path, &result)?;
    if gate {
        ensure!(
            result
                .trim()
                .lines()
                .last()
                .is_some_and(|s| s.trim() == "AEP: PASS"),
            "验收尚未通过，请查看阶段答复并补齐证据"
        );
    }
    let _lock = stop.lock()?;
    stop.check()?;
    let author = format!(
        "{} / {}",
        profile.client.label(),
        if profile.model.is_empty() {
            "客户端默认"
        } else {
            &profile.model
        }
    );
    let summary = format!("{} · {}\n{}", req.work, stage.name, result.trim())
        .chars()
        .take(TEXT_LIMIT)
        .collect::<String>();
    record["written_memory"] = json!(memory.project_handoff(id, &author, &summary)?);
    record["status"] = json!("succeeded");
    record["finished_at"] = json!(chrono::Utc::now().to_rfc3339());
    save(dir, record)
}

pub fn execute(request: &Path) -> Result<()> {
    let request = request.canonicalize()?;
    let dir = request.parent().context("missing attempt directory")?;
    let work = dir
        .parent()
        .and_then(Path::parent)
        .context("missing work directory")?;
    let id = dir
        .file_name()
        .and_then(|v| v.to_str())
        .context("missing attempt ID")?;
    let req: Request = serde_json::from_reader(fs::File::open(&request)?)?;
    let stage = req
        .template
        .stages()
        .get(req.stage)
        .context("unknown workflow stage")?;
    let stop = Cancellation::new(work, id)?;
    let lock = |p: PathBuf| {
        fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(p)
    };
    let owner = lock(dir.join("owner.lock"))?;
    owner.try_lock().context("此执行已运行")?;
    ensure!(
        !dir.join("evidence.json").exists(),
        "此执行已有记录，请新建重试"
    );
    let mut record = json!({"request":"request.json","run_id":id,"status":"running","stage":stage.name,"profile":req.profile,"started_at":chrono::Utc::now().to_rfc3339()});
    save(dir, &record)?;
    let result = lock(work.join("workflow.lock"))
        .map_err(anyhow::Error::from)
        .and_then(|exclusive| {
            exclusive.try_lock().context("此事项已有 CLI 正在运行")?;
            perform(&req, stage, dir, work, id, &stop, &mut record)
        });
    if let Err(e) = &result {
        let _guard = stop.lock()?;
        record["status"] = json!(if stop.requested()? {
            "cancelled"
        } else {
            "failed"
        });
        record["error"] = json!(format!("{e:#}"));
        record["finished_at"] = json!(chrono::Utc::now().to_rfc3339());
        save(dir, &record)?;
    }
    result
}
