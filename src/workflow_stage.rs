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

fn workspace(
    work: &Path,
    repository: Option<&Path>,
    delivery: Option<&aep::project::transfer::Transfer>,
    attempt: &str,
) -> Result<PathBuf> {
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
    let delivery = delivery.filter(|t| t.repository.as_ref() == Some(&repo.root));
    let pinned = delivery.and_then(|t| t.candidate.as_deref());
    let existing = delivery.and_then(|t| t.checkout.as_deref());
    let checkout = if let Some(saved) = existing {
        aep::workspace::ident(saved, 160)?;
        ensure!(
            saved == id || saved.starts_with(&format!("{id}-")),
            "前序工作区属于其他事项"
        );
        saved.to_owned()
    } else if pinned.is_some() {
        format!("{id}-{attempt}")
    } else {
        id.to_owned()
    };
    let path = repo.data.join("workflow-worktrees").join(&checkout);
    ensure!(existing.is_none() || path.is_dir(), "前序阶段工作区已移除");
    let branch = format!("aep-stage/{checkout}");
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
                pinned.unwrap_or("HEAD"),
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
pub(crate) fn context(
    req: &Request,
    dir: &Path,
    cwd: &Path,
    since: Option<i64>,
) -> Result<aep::handoff::Packet> {
    let work = dir
        .parent()
        .and_then(Path::parent)
        .context("missing work")?;
    let mut packet = preview(req, work, cwd, since)?;
    if since.is_none() {
        packet.text.push_str(&format!(
            "\n本次档案 {}。前序执行的 result.txt 和 evidence.json 位于同级执行目录。",
            dir.display()
        ));
    }
    Ok(packet)
}
pub(crate) fn preview(
    req: &Request,
    work: &Path,
    cwd: &Path,
    since: Option<i64>,
) -> Result<aep::handoff::Packet> {
    let memory = Memory::open(work.parent().context("missing project directory")?)?;
    let previous = req.prior_runs.last().map(|(_, id)| id.as_str());
    if let Some(cursor) = since {
        ensure!(cursor >= 0, "记忆游标必须大于等于零");
        return aep::handoff::packet(req, &memory.workflow_delta(previous, cursor)?, &[], since);
    }
    let key = Key::new("", "", "");
    let notes = memory.workflow_context(previous)?;
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
                        &aep::workspace::commit(cwd, "HEAD").unwrap_or_default(),
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
    aep::handoff::packet(req, &notes, &repo_notes, None)
}
/// Pin the exact upstream receipt alongside the prompt for a downstream gate or delivery.
fn source_receipt(req: &Request, stage: &Stage, dir: &Path) -> Result<Option<PathBuf>> {
    let Some(t) = &req.stage_transfer else {
        return Ok(None);
    };
    if !matches!(stage.role, Role::Accept | Role::Deliver)
        || !req
            .prior_runs
            .iter()
            .any(|(i, id)| *i == t.from && id == &t.run)
    {
        return Ok(None);
    }
    aep::workspace::ident(&t.run, 64)?;
    let runs = dir.parent().context("找不到事项执行目录")?;
    let raw = fs::read(runs.join(&t.run).join("evidence.json"))?;
    let ev: Value = serde_json::from_slice(&raw)?;
    ensure!(
        t.to == req.stage
            && t.from.checked_add(1) == Some(req.stage)
            && ev["run_id"] == t.run
            && ev["status"] == "succeeded",
        "前序回执与已确认的交接不符"
    );
    let inputs = dir.join("inputs");
    fs::create_dir_all(&inputs)?;
    let file = inputs.join("source-evidence.json");
    fs::write(&file, raw)?;
    Ok(Some(file))
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
    let gate = matches!(stage.role, Role::Test | Role::Accept);
    let write = stage.role == Role::Build;
    let prior = req
        .follow_up
        .as_ref()
        .map(|f| prior_turn(dir, f))
        .transpose()?;
    let cwd = match &prior {
        Some(p) => p.workspace.clone(),
        None => workspace(
            work,
            req.repository.as_deref(),
            req.stage_transfer.as_ref(),
            id,
        )?,
    };
    // A native session already holds the stage context; send only the new message.
    let resume = prior
        .as_ref()
        .filter(|_| matches!(req.profile.client, Client::Codex | Client::Claude))
        .and_then(|p| p.session.clone());
    if let (Some(p), Some(f)) = (&prior, &req.follow_up) {
        record["follow_up"] = json!({"attempt":f.attempt,"resumed_session":resume});
        if resume.is_some() {
            let mut prompt = f.message.clone();
            if gate {
                prompt.push_str(
                    "\n\n（如果结论有变化，答复最后一行仍按约定：通过时单独输出 AEP: PASS。）",
                );
            }
            match invoke(
                req,
                stage,
                dir,
                work,
                id,
                stop,
                record,
                &cwd,
                prompt,
                None,
                resume,
                gate,
                write,
                (vec![], vec![]),
            ) {
                Err(e) if !stop.requested()? && session_missing(dir) => {
                    // Fall back to sending the context and the previous answer.
                    record["follow_up"]["resume_error"] = json!(format!("{e:#}"));
                    record["follow_up"]["resumed_session"] = Value::Null;
                    record["session"] = Value::Null;
                }
                other => return other,
            }
        }
        let packet = context(req, dir, &cwd, None)?;
        let prompt = format!(
            "{}\n\n## 上一轮答复\n{}\n\n## 继续的要求\n{}",
            packet.text,
            p.result.chars().take(8000).collect::<String>(),
            f.message
        );
        return invoke(
            req,
            stage,
            dir,
            work,
            id,
            stop,
            record,
            &cwd,
            prompt,
            None,
            None,
            gate,
            write,
            (packet.project_ids, packet.repo_ids),
        );
    }
    let packet = context(req, dir, &cwd, None)?;
    let mut prompt = packet.text;
    let receipt = source_receipt(req, stage, dir)?;
    if let Some(file) = &receipt {
        prompt.push_str(&format!(
            "\n请读取前序阶段的原始执行回执 {}，核对来源会话、结论和决策，再形成自己的结论。",
            file.display()
        ));
        if stage.role == Role::Accept {
            prompt.push_str("请在独立验收报告中引用你从该回执读到的完整 run_id，并列出决策编号和一项具体测试结果。");
        }
        record["source_evidence"] = json!({"run":req.stage_transfer.as_ref().map(|t|&t.run),"file":"inputs/source-evidence.json"});
    }
    invoke(
        req,
        stage,
        dir,
        work,
        id,
        stop,
        record,
        &cwd,
        prompt,
        receipt,
        None,
        gate,
        write,
        (packet.project_ids, packet.repo_ids),
    )
}

struct Prior {
    workspace: PathBuf,
    session: Option<String>,
    result: String,
}
/// Load what a follow-up continues: the latest workspace, the newest session that was
/// actually saved, and the newest non-empty answer. A failed round (for example a resume
/// that could not find its session) defers to the round it continued.
fn prior_turn(dir: &Path, follow: &aep::workflow::FollowUp) -> Result<Prior> {
    let runs = dir.parent().context("missing runs directory")?;
    let mut id = follow.attempt.clone();
    let mut workspace = None;
    let mut session = None;
    let mut result = String::new();
    for _ in 0..64 {
        aep::workspace::ident(&id, 64)?;
        let previous = runs.join(&id);
        let ev = aep::workflow::evidence(&previous)?;
        if workspace.is_none() {
            workspace = ev["workspace"].as_str().map(PathBuf::from);
        }
        // Runs from before sessions were persisted (`--ephemeral`) cannot be resumed.
        if session.is_none() && ev["resumable"] == true {
            session = ev["session"].as_str().map(str::to_owned);
        }
        if result.trim().is_empty() {
            result = fs::read_to_string(previous.join("result.txt")).unwrap_or_default();
        }
        if session.is_some() && !result.trim().is_empty() {
            break;
        }
        match ev["follow_up"]["attempt"].as_str() {
            Some(next) => id = next.to_owned(),
            None => break,
        }
    }
    let workspace = workspace.context("上一轮执行没有工作区")?;
    ensure!(workspace.is_dir(), "上一轮执行的工作区已移除，请重新执行");
    Ok(Prior {
        workspace,
        session,
        result,
    })
}

/// The CLI could not find the session it was asked to resume (pruned or never saved).
fn session_missing(dir: &Path) -> bool {
    ["stderr.log", "stdout.log"].iter().any(|name| {
        let log = tail(&dir.join(name), 4000).to_lowercase();
        log.contains("no rollout found")
            || log.contains("no conversation found")
            || log.contains("thread/resume failed")
    })
}

/// Claude's stream-json output ends with one `result` event; older single-object
/// output (and test doubles) carry the same fields without a `type`.
fn claude_result(stdout: &Path) -> Result<Value> {
    let mut last = None;
    for line in BufReader::new(fs::File::open(stdout)?).lines() {
        if let Ok(v) = serde_json::from_str::<Value>(&line?)
            && (v["type"] == "result" || (v["type"].is_null() && !v["result"].is_null()))
        {
            last = Some(v);
        }
    }
    last.context("Claude 未生成最终答复")
}

#[allow(clippy::too_many_arguments)]
fn invoke(
    req: &Request,
    stage: &Stage,
    dir: &Path,
    work: &Path,
    id: &str,
    stop: &Cancellation,
    record: &mut Value,
    cwd: &Path,
    prompt: String,
    receipt: Option<PathBuf>,
    resume: Option<String>,
    gate: bool,
    write: bool,
    (project_ids, repo_ids): (Vec<i64>, Vec<i64>),
) -> Result<()> {
    let cwd = cwd.to_path_buf();
    fs::write(dir.join("prompt.txt"), &prompt)?;
    if req.follow_up.is_none()
        && let Some(t) = &req.stage_transfer
    {
        record["read_stage_transfer"] = json!(t);
    }
    record["workspace"] = json!(cwd);
    record["prompt_bytes"] = json!(prompt.len());
    record["read_memory"] = json!(project_ids);
    record["read_repository_memory"] = json!(repo_ids);
    save(dir, record)?;
    let profile = &req.profile;
    let bin = profile.program(&std::env::var_os("PATH").unwrap_or_default())?;
    let bundled_zcode = bin == Path::new(aep::workflow::ZCODE_APP)
        || (profile.client == Client::ZCode
            && bin
                .canonicalize()
                .is_ok_and(|p| p == Path::new(aep::workflow::ZCODE_APP)));
    let bridge = profile.client == Client::ZCode && (bundled_zcode || !profile.model.is_empty());
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
            let sandbox = if write || gate {
                "workspace-write"
            } else {
                "read-only"
            };
            args.push("exec".into());
            if resume.is_some() {
                // `exec resume` has no --sandbox flag; the config key sets the same policy.
                args.extend([
                    "resume".into(),
                    "-c".into(),
                    format!("sandbox_mode=\"{sandbox}\""),
                ]);
            } else {
                args.extend(["--sandbox".into(), sandbox.into()]);
            }
            args.extend(
                ["--skip-git-repo-check", "--json", "--output-last-message"].map(String::from),
            );
            args.push(result_path.display().to_string());
        }
        Client::Claude => {
            if let Some(parent) = receipt.as_ref().and_then(|p| p.parent()) {
                args.extend(["--add-dir".into(), parent.display().to_string()]);
            }
            args.extend(
                [
                    "-p",
                    "--output-format",
                    "stream-json",
                    "--verbose",
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
        Client::ZCode if bridge => {
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
    if let Some(session) = &resume {
        if profile.client == Client::Claude {
            args.extend(["--resume".into(), session.clone()]);
        } else {
            args.push(session.clone());
        }
    }
    if profile.client == Client::Codex {
        args.push("-".into());
    }
    if profile.client == Client::ZCode && !bridge {
        args.push(dir.join("prompt.txt").display().to_string());
    }
    let stdout = dir.join("stdout.log");
    let stderr = dir.join("stderr.log");
    let code = logged(
        &args,
        &cwd,
        &stdout,
        // Stage sessions run until they finish or the user cancels them.
        u64::MAX,
        (profile.client != Client::ZCode).then_some(prompt.as_str()),
        Some(stderr.clone()),
        stop,
    )?;
    record["exit_code"] = json!(code);
    // Codex and Claude now keep their sessions on disk, so this run can be continued.
    record["resumable"] = json!(matches!(profile.client, Client::Codex | Client::Claude));
    if dir.join("session.json").is_file() {
        let native = aep::workspace::read(&dir.join("session.json"))?;
        record["native_session"] = native;
    }
    if profile.client == Client::Codex {
        record["commands"] = json!(command_receipts(&stdout)?);
        for line in BufReader::new(fs::File::open(&stdout)?).lines().take(20) {
            if let Ok(event) = serde_json::from_str::<Value>(&line?)
                && event["type"] == "thread.started"
                && event["thread_id"].is_string()
            {
                record["session"] = event["thread_id"].clone();
                break;
            }
        }
    }
    if record["session"].is_null()
        && let Some(session) = &resume
    {
        record["session"] = json!(session);
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
            let v = claude_result(&stdout)?;
            record["session"] = v["session_id"].clone();
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
    let mut memory = Memory::open(work.parent().context("missing project directory")?)?;
    let summary = format!(
        "{} · {}\n{}",
        req.work,
        stage.name,
        aep::handoff::summary(&result)
    )
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
    ensure!(
        req.external.is_none(),
        "外部会话已领取此阶段，请用 project submit 交回结果"
    );
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

/// Read native counters without conflating cached and uncached tokens across providers.
pub(crate) fn metrics(dir: &Path) -> Result<Value> {
    let e = aep::workflow::evidence(dir)?;
    let profile = &e["profile"];
    let elapsed = e["started_at"]
        .as_str()
        .zip(e["finished_at"].as_str())
        .and_then(|(a, b)| {
            Some(
                (chrono::DateTime::parse_from_rfc3339(b).ok()?
                    - chrono::DateTime::parse_from_rfc3339(a).ok()?)
                .num_milliseconds(),
            )
        });
    let mut usage = serde_json::Map::new();
    if e["mode"] != "external"
        && matches!(profile["client"].as_str(), Some("Codex" | "Claude"))
        && dir.join("stdout.log").is_file()
    {
        for line in BufReader::new(fs::File::open(dir.join("stdout.log"))?).lines() {
            if let Ok(value) = serde_json::from_str::<Value>(&line?)
                && (value["type"] == "turn.completed" || value["type"] == "result")
                && let Some(row) = value["usage"].as_object()
            {
                for (k, v) in row {
                    if let Some(n) = v.as_u64()
                        && k.ends_with("tokens")
                    {
                        let prev = usage.get(k).and_then(Value::as_u64).unwrap_or(0);
                        usage.insert(k.clone(), json!(prev.saturating_add(n)));
                    }
                }
            }
        }
    }
    Ok(
        json!({"attempt":e["run_id"],"stage":e["stage"],"client":profile["client"],"status":e["status"],"mode":e["mode"].as_str().unwrap_or("managed"),"elapsed_ms":elapsed,"prompt_bytes":dir.join("prompt.txt").metadata().ok().map(|m|m.len()),"provider_usage":(!usage.is_empty()).then_some(usage)}),
    )
}
