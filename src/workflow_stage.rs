use crate::{agent::logged, run::save};
use aep::{
    cancel::Cancellation,
    memory::{Key, Memory, TEXT_LIMIT},
    workflow::{Client, Profile, Request, Role, Sandbox, Stage, tail},
    workspace::{Workspace, git},
    zcode_receipt::ActualModel,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

fn handoff_author(profile: &Profile, actual: Option<&ActualModel>) -> String {
    if profile.client == Client::ZCode
        && let Some(actual) = actual
    {
        return actual.author();
    }
    format!(
        "{} / {}",
        profile.client.label(),
        if profile.model.is_empty() {
            "客户端默认"
        } else {
            &profile.model
        }
    )
}

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
    // Planning stages write the spec documents; the build writes code.
    let write = matches!(stage.role, Role::Build | Role::Plan);
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
        // A re-check after a send-back continues the session but consumes the new hand-off:
        // pin the upstream receipt again so delivery can prove which acceptance it read.
        let mut message = f.message.clone();
        if let Some(file) = source_receipt(req, stage, dir)? {
            message.push_str(&format!(
                "\n\n前序阶段的最新原始执行回执：{}，请核对后再形成结论。",
                file.display()
            ));
            record["source_evidence"] = json!({"run":req.stage_transfer.as_ref().map(|t|&t.run),"file":"inputs/source-evidence.json"});
        }
        if resume.is_some() {
            let mut prompt = message.clone();
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
            message
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

/// Files that differ from HEAD (tracked or untracked), with spec contents kept for restore.
struct Snapshot {
    hashes: BTreeMap<String, u64>,
    specs: BTreeMap<String, Vec<u8>>,
}
const SPECS: &str = "specs/";

fn snapshot(cwd: &Path) -> Option<Snapshot> {
    use std::hash::{Hash, Hasher};
    let out = git(
        cwd,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )
    .ok()?;
    let mut snap = Snapshot {
        hashes: BTreeMap::new(),
        specs: BTreeMap::new(),
    };
    let mut records = out.split('\0');
    while let Some(rec) = records.next() {
        if rec.len() < 4 {
            continue;
        }
        let (status, path) = rec.split_at(3);
        if status.starts_with('R') || status.starts_with('C') {
            records.next();
        }
        let bytes = fs::read(cwd.join(path)).ok();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut h);
        snap.hashes.insert(path.to_owned(), h.finish());
        if path.starts_with(SPECS)
            && let Some(b) = bytes
        {
            snap.specs.insert(path.to_owned(), b);
        }
    }
    Some(snap)
}

/// Only planning stages own `specs/`. A planning stage that touched other files fails; any
/// other stage's spec edits move into this run's archive and `specs/` is put back.
fn documents(
    stage: &Stage,
    dir: &Path,
    cwd: &Path,
    before: &Snapshot,
    record: &mut Value,
) -> Result<()> {
    let Some(after) = snapshot(cwd) else {
        return Ok(());
    };
    let changed: Vec<String> = before
        .hashes
        .keys()
        .chain(after.hashes.keys())
        .filter(|p| before.hashes.get(*p) != after.hashes.get(*p))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let (specs, other): (Vec<_>, Vec<_>) = changed.into_iter().partition(|p| p.starts_with(SPECS));
    if stage.role == Role::Plan {
        record["spec_files"] = json!(specs);
        ensure!(
            other.is_empty(),
            "规划阶段只能修改 specs/ 下的文档，本轮还改了：{}",
            other.iter().take(8).cloned().collect::<Vec<_>>().join("、")
        );
        return Ok(());
    }
    for path in &specs {
        let file = cwd.join(path);
        if let Ok(bytes) = fs::read(&file) {
            let draft = dir.join("spec-drafts").join(path);
            fs::create_dir_all(draft.parent().context("draft path")?)?;
            fs::write(draft, bytes)?;
        }
        if let Some(original) = before.specs.get(path) {
            fs::write(&file, original)?;
        } else if git(cwd, &["cat-file", "-e", &format!("HEAD:{path}")]).is_ok() {
            git(cwd, &["checkout", "HEAD", "--", path])?;
        } else {
            let _ = fs::remove_file(&file);
        }
    }
    if !specs.is_empty() {
        record["spec_drafts"] = json!(specs);
    }
    Ok(())
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
    // Follow-ups carry the stage's current incoming hand-off; a re-check is how a resumed
    // session reads a new one, so it is recorded the same way as a fresh brief.
    if let Some(t) = &req.stage_transfer {
        record["read_stage_transfer"] = json!(t);
    }
    record["workspace"] = json!(cwd);
    record["sandbox"] = json!(req.sandbox.label());
    record["prompt_bytes"] = json!(prompt.len());
    record["read_memory"] = json!(project_ids);
    record["read_repository_memory"] = json!(repo_ids);
    save(dir, record)?;
    let profile = &req.profile;
    let bin = profile.program(&std::env::var_os("PATH").unwrap_or_default())?;
    let mut args = vec![bin.display().to_string()];
    let result_path = dir.join("result.txt");
    match profile.client {
        Client::Codex => {
            let sandbox = match req.sandbox {
                Sandbox::Full => "danger-full-access",
                _ if write || gate => "workspace-write",
                _ => "read-only",
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
            if req.sandbox == Sandbox::Network && sandbox == "workspace-write" {
                // Test suites listen on local ports; the default workspace sandbox has no network.
                args.extend([
                    "-c".into(),
                    "sandbox_workspace_write.network_access=true".into(),
                ]);
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
                    match req.sandbox {
                        Sandbox::Full => "bypassPermissions",
                        _ if write => "acceptEdits",
                        _ => "default",
                    },
                    "--tools",
                    if write {
                        "Read,Grep,Glob,Edit,Write,Bash"
                    } else {
                        "Read,Grep,Glob,Bash"
                    },
                ]
                .map(String::from),
            );
            if req.sandbox == Sandbox::Network {
                // Print mode cannot ask for approval; let the stage run its test commands.
                args.extend(["--allowedTools".into(), "Bash".into()]);
            }
        }
        Client::ZCode => {
            args = vec![
                std::env::current_exe()?.display().to_string(),
                "zcode-stage".into(),
                "--binary".into(),
                bin.display().to_string(),
                "--request".into(),
                dir.join("request.json").display().to_string(),
            ];
        }
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
    let stdout = dir.join("stdout.log");
    let stderr = dir.join("stderr.log");
    let before = snapshot(&cwd);
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
    if let Some(before) = &before {
        documents(stage, dir, &cwd, before, record)?;
    }
    // Codex and Claude now keep their sessions on disk, so this run can be continued.
    record["resumable"] = json!(matches!(profile.client, Client::Codex | Client::Claude));
    let actual = if profile.client == Client::ZCode {
        match aep::workspace::read(&dir.join("session.json")) {
            Ok(native) => {
                let actual = ActualModel::from_receipt(&native);
                record["native_session"] = native;
                actual
            }
            Err(error) => {
                if dir.join("session.json").exists() {
                    eprintln!("实际模型回执无法读取：{error:#}");
                }
                None
            }
        }
    } else {
        None
    };
    record["actual_model"] = json!(actual);
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

    if code != 0 {
        // The CLI's own account of the failure beats stderr noise (e.g. MCP login warnings).
        let reported = fs::read_to_string(&stdout)
            .ok()
            .and_then(|raw| aep::transcript::failure(&raw));
        anyhow::bail!(
            "{} 退出码 {}\n{}",
            profile.client.label(),
            code,
            match reported {
                Some(message) => message,
                None if stderr.metadata()?.len() > 0 => tail(&stderr, 2400),
                None => tail(&stdout, 2400),
            }
        );
    }
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
    let skips = aep::workflow::skips(req.template, req.stage, &result);
    if !skips.is_empty() {
        record["skip"] = json!(
            skips
                .iter()
                .map(|(s, r)| json!({"stage":s,"name":req.template.stages()[*s].name,"reason":r}))
                .collect::<Vec<_>>()
        );
    }
    // An earlier stage must act first: record where to and why, and do not let this pass.
    if let Some((to, question)) = aep::workflow::route_back(req.template, req.stage, &result) {
        let target = req.template.stages()[to].name;
        record["route_back"] = json!({"stage":to,"name":target,"question":question});
        anyhow::bail!("需要「{target}」先解决：{question}");
    }
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
    let author = handoff_author(profile, actual.as_ref());
    pin_worktree(req, stage, &cwd, id, record)?;
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

/// Commit what a planning or build stage wrote onto the item branch, and pin every passing
/// stage's HEAD, so delivery merges exactly the commit independent acceptance approved.
fn pin_worktree(
    req: &Request,
    stage: &Stage,
    cwd: &Path,
    id: &str,
    record: &mut Value,
) -> Result<()> {
    let Some(repo) = &req.repository else {
        return Ok(());
    };
    if record["mode"] == "external" {
        return Ok(());
    }
    let repo = Workspace::open(repo)?;
    let trees = repo.data.join("workflow-worktrees");
    if cwd.canonicalize()?.parent() != Some(trees.canonicalize()?.as_path()) {
        return Ok(());
    }
    let status = |cwd: &Path| git(cwd, &["status", "--porcelain", "--untracked-files=all"]);
    if matches!(stage.role, Role::Plan | Role::Build) && !status(cwd)?.trim().is_empty() {
        git(cwd, &["add", "-A"])?;
        let message = format!("{} · {}\n\nAEP-Run: {id}", req.work, stage.name);
        let named = git(cwd, &["config", "user.email"]).is_ok_and(|e| !e.trim().is_empty());
        let mut args = vec![];
        if !named {
            args.extend([
                "-c",
                "user.name=AEP Studio",
                "-c",
                "user.email=aep-studio@localhost",
            ]);
        }
        args.extend(["commit", "--no-verify", "-q", "-m", &message]);
        git(cwd, &args)?;
        record["committed"] = json!(true);
    }
    let head = aep::workspace::commit(cwd, "HEAD")?;
    git(
        &repo.root,
        &["update-ref", &format!("refs/aep/stages/{id}"), &head],
    )?;
    record["head_sha"] = json!(head);
    // Untracked leftovers (reports, caches) are not part of the commit and do no harm; a
    // changed tracked file would mean the reviewed code is not the committed code.
    let changed = git(cwd, &["status", "--porcelain", "--untracked-files=no"])?;
    record["clean"] = json!(changed.trim().is_empty());
    let untracked = status(cwd)?.lines().filter(|l| l.starts_with("??")).count();
    if untracked > 0 {
        record["untracked"] = json!(untracked);
    }
    ensure!(
        changed.trim().is_empty(),
        "「{}」改动了事项工作区里的已提交文件，验收类阶段不应修改代码；需要修复请用 AEP-BACK 交给开发实现：\n{}",
        stage.name,
        changed.lines().take(10).collect::<Vec<_>>().join("\n")
    );
    Ok(())
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
    let mut record = json!({"request":"request.json","run_id":id,"status":"running","stage":stage.name,"profile":req.profile,"actual_model":null,"started_at":chrono::Utc::now().to_rfc3339()});
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zcode_handoff_author_uses_the_receipt_not_the_selected_model() {
        let mut profile = Profile {
            client: Client::ZCode,
            model: "selected/glm".into(),
            executable: String::new(),
            check_command: String::new(),
        };
        let actual = ActualModel::from_receipt(&json!({
            "transport":"desktop","providerId":"account:bigmodel-start-plan","modelId":"glm"
        }))
        .unwrap();
        assert_eq!(
            handoff_author(&profile, Some(&actual)),
            "ZCode / BigModel Start Plan / glm@默认"
        );
        assert_eq!(handoff_author(&profile, None), "ZCode / selected/glm");
        profile.client = Client::Claude;
        assert_eq!(
            handoff_author(&profile, Some(&actual)),
            "Claude / selected/glm"
        );
    }
}
