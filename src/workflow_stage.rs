use crate::{agent::logged, run::save};
use aep::{
    cancel::Cancellation,
    flow::Back,
    memory::{Key, Memory, TEXT_LIMIT},
    session_reuse::{self, Mode, Plan, Reason, Side},
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
    // Stage worktrees live under the Studio catalog, outside `.git`: Claude Code refuses to
    // write anywhere inside `.git`. The catalog is three levels above the item folder.
    let catalog = work.ancestors().nth(3).context("missing Studio catalog")?;
    let path = aep::project::branches::worktree_home(catalog, &repo.root).join(&checkout);
    let branch = format!("aep-stage/{checkout}");
    if !path.exists()
        && let Some((_, old)) = aep::project::branches::stage_worktrees(&repo.root)
            .into_iter()
            .find(|(name, p)| *name == checkout && p.is_dir())
    {
        // Created before worktrees moved out of `.git`: move it, uncommitted work and all.
        fs::create_dir_all(path.parent().unwrap())?;
        git(
            &repo.root,
            &[
                "worktree",
                "move",
                old.to_str().context("invalid workspace path")?,
                path.to_str().context("invalid workspace path")?,
            ],
        )?;
    }
    ensure!(existing.is_none() || path.is_dir(), "前序阶段工作区已移除");
    if !path.exists() {
        fs::create_dir_all(path.parent().unwrap())?;
        let target = path.to_str().context("invalid workspace path")?;
        // An item imported from an existing branch already has its branch; check it out.
        let prepared = pinned.is_none()
            && git(
                &repo.root,
                &["rev-parse", "--verify", &format!("refs/heads/{branch}")],
            )
            .is_ok();
        if prepared {
            git(&repo.root, &["worktree", "add", target, &branch])?;
        } else {
            git(
                &repo.root,
                &[
                    "worktree",
                    "add",
                    "-b",
                    &branch,
                    target,
                    pinned.unwrap_or("HEAD"),
                ],
            )?;
        }
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
    let mut packet = context_packet(req, work, cwd, since)?;
    if since.is_none() {
        packet.text.push_str(&format!(
            "\n本次档案 {}。前序执行的 result.txt 和 evidence.json 位于同级执行目录。",
            dir.display()
        ));
    }
    Ok(packet)
}
pub(crate) fn preview(
    req: &Result<Request>,
    work: &Path,
    cwd: &Path,
    since: Option<i64>,
) -> Result<aep::handoff::Packet> {
    let req = req.as_ref().map_err(|err| anyhow::anyhow!("{err:#}"))?;
    context_packet(req, work, cwd, since)
}

fn context_packet(
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

/// Record how the session was chosen: why a new one started, and the context that was
/// compared with the resume limit.
fn record_plan(record: &mut Value, plan: &Plan, threshold: u64) {
    let context = match plan {
        Plan::Resume { context, .. } => *context,
        Plan::Fresh { reason, context } => {
            if *reason == Reason::ClientNotReusable {
                return;
            }
            record["fresh_session_reason"] = json!(reason.as_str());
            *context
        }
    };
    if let Some(c) = context {
        record["resume_context"] = json!(c);
        record["resume_threshold"] = json!(threshold);
    }
}

/// A resume the CLI could not perform (session pruned or never saved, E4): note it and let
/// the caller start a new session with the hand-off digest instead.
fn resume_failed(record: &mut Value, error: &anyhow::Error) {
    record["fresh_session_reason"] = json!(Reason::SessionUnavailable.as_str());
    record["resume_error"] = json!(format!("{error:#}"));
    record["reused_session"] = Value::Null;
    record["session"] = Value::Null;
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
    let threshold = session_reuse::limit(req.resume_limit);
    let runs = dir.parent().context("missing runs directory")?;
    if let Some(f) = &req.follow_up {
        // A stage worktree moved out of `.git` since that turn is found again (main ad25597).
        let p = prior_turn(dir, f, req.repository.as_deref())?;
        let cwd = p.workspace.clone();
        if let Some(note) = align_base(req, stage, &cwd, id, record)? {
            record["base"]["note"] = json!(note);
        }
        let current = Side::of(req, Some(&cwd));
        // A native session already holds the stage context; send only the new message,
        // unless it has grown past the resume limit (R2).
        let plan = match &p.session_run {
            Some(run) => match session_reuse::source(runs, req.stage, &stage.name, run) {
                Ok(src) => session_reuse::decide(Mode::Continue, &current, Some(&src), threshold),
                Err(_) => session_reuse::decide(Mode::Continue, &current, None, threshold),
            },
            None => session_reuse::decide(Mode::Continue, &current, None, threshold),
        };
        // ZCode is outside `decide` (no adjacent reuse), but its app-server sessions can be
        // continued by a follow-up (R7); desktop tasks keep re-running.
        let plan = if req.profile.client == Client::ZCode {
            zcode_continue(runs, req.stage, &p)
        } else {
            plan
        };
        let plan = match plan {
            // The round being continued exists; what is missing is a session to resume.
            Plan::Fresh {
                reason: Reason::NoPrior,
                context,
            } => Plan::Fresh {
                reason: Reason::SessionUnavailable,
                context,
            },
            other => other,
        };
        record_plan(record, &plan, threshold);
        let resume = match &plan {
            Plan::Resume { session, .. } => Some(session.clone()),
            Plan::Fresh { .. } => None,
        };
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
        if let Some(session) = resume {
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
                Some((session, false)),
                gate,
                write,
                (vec![], vec![]),
            ) {
                Err(e) if !stop.requested()? && session_missing(dir) => {
                    // Fall back to sending the context and the previous answer.
                    record["follow_up"]["resume_error"] = json!(format!("{e:#}"));
                    record["follow_up"]["resumed_session"] = Value::Null;
                    resume_failed(record, &e);
                }
                other => return other,
            }
        }
        let packet = context(req, dir, &cwd, None)?;
        aep::workspace::ident(&f.attempt, 64)?;
        let prompt = aep::handoff::with_previous(
            &packet.text,
            &p.result,
            &runs.join(&f.attempt).join("evidence.json"),
            Some(&message),
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
    let cwd = workspace(
        work,
        req.repository.as_deref(),
        req.stage_transfer.as_ref(),
        id,
    )?;
    if let Some(note) = align_base(req, stage, &cwd, id, record)? {
        record["base"]["note"] = json!(note);
    }
    // Reuse the previous stage's session when it ran the same CLI, model, permissions and
    // workspace (R3); `prior_runs` already leaves out skipped stages (E6).
    let current = Side::of(req, Some(&cwd));
    // A build subtask continues from the subtask before it, which has just read and changed
    // the same code; the first one continues from the design stage.
    let source = match (&req.subtask, req.subtasks_done.last()) {
        (Some(_), Some(done)) => Some(session_reuse::source(
            runs,
            req.stage,
            &format!("{} · {}", stage.name, done.id),
            &done.run,
        )),
        _ => req.prior_runs.last().map(|(s, run)| {
            let name = req.flow().stages.get(*s).map_or("", |st| st.name.as_str());
            session_reuse::source(runs, *s, name, run)
        }),
    };
    let plan = match &source {
        Some(Err(_)) if matches!(req.profile.client, Client::Codex | Client::Claude) => {
            Plan::Fresh {
                reason: Reason::SessionUnavailable,
                context: None,
            }
        }
        Some(Ok(src)) => session_reuse::decide(Mode::Adjacent, &current, Some(src), threshold),
        _ => session_reuse::decide(Mode::Adjacent, &current, None, threshold),
    };
    record_plan(record, &plan, threshold);
    let source = source.and_then(Result::ok);
    let receipt = source_receipt(req, stage, dir)?;
    let receipt_text = |prompt: &mut String, record: &mut Value| {
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
    };
    let mut carry = matches!(&plan, Plan::Fresh { reason, .. } if reason.carries_previous());
    if let (
        Plan::Resume {
            session,
            fork,
            source_stage,
            source_run,
            ..
        },
        Some(src),
    ) = (&plan, &source)
    {
        record["reused_session"] = json!({"stage":source_stage,"name":src.stage_name,"run":source_run,"session":session,"forked":fork});
        // The session already holds memory and earlier outputs: send the short prompt.
        let mut prompt = aep::handoff::adjacent(req, dir)?;
        receipt_text(&mut prompt, record);
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
            receipt.clone(),
            Some((session.clone(), *fork)),
            gate,
            write,
            (vec![], vec![]),
        ) {
            Err(e) if !stop.requested()? && session_missing(dir) => {
                resume_failed(record, &e);
                carry = true;
            }
            other => return other,
        }
    }
    let packet = context(req, dir, &cwd, None)?;
    let mut prompt = packet.text;
    receipt_text(&mut prompt, record);
    // The source session exists but is too large or cannot be continued: start a new one
    // that carries its answer and hand-off digest (R2, E4, E5).
    if carry && let Some(src) = &source {
        let src_dir = runs.join(&src.run);
        let previous = fs::read_to_string(src_dir.join("result.txt")).unwrap_or_default();
        prompt =
            aep::handoff::with_previous(&prompt, &previous, &src_dir.join("evidence.json"), None);
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

/// Whether `cwd` is one of this repository's stage worktrees (found by branch, not path).
fn stage_worktree(root: &Path, cwd: &Path) -> bool {
    let Ok(cwd) = cwd.canonicalize() else {
        return false;
    };
    aep::project::branches::stage_worktrees(root)
        .iter()
        .any(|(_, p)| p.canonicalize().is_ok_and(|p| p == cwd))
}

/// Git identity flags for platform commits when the repository has none configured.
fn identity(cwd: &Path) -> Vec<&'static str> {
    if git(cwd, &["config", "user.email"]).is_ok_and(|e| !e.trim().is_empty()) {
        vec![]
    } else {
        vec![
            "-c",
            "user.name=AEP Studio",
            "-c",
            "user.email=aep-studio@localhost",
        ]
    }
}

/// Keep the item branch merged with its base (the branch checked out in the repository's main
/// worktree, usually main), so a moved base shows up at the next stage, not at delivery.
///
/// - Writing stages and the test stage merge a base that merges cleanly before they start.
/// - A conflicting merge is left to the build: it is told which files conflict.
/// - Acceptance and delivery check an exact tested commit, so they do not merge: a moved base
///   sends the item back to the test stage (clean) or the build (conflicts) without a model call.
///
/// Returns the note the stage's prompt gets, if any.
fn align_base(
    req: &Request,
    stage: &Stage,
    cwd: &Path,
    id: &str,
    record: &mut Value,
) -> Result<Option<String>> {
    let Some(repo) = &req.repository else {
        return Ok(None);
    };
    let root = Workspace::open(repo)?.root;
    if !stage_worktree(&root, cwd) {
        return Ok(None);
    }
    let Some(base) = git(&root, &["symbolic-ref", "--short", "-q", "HEAD"])
        .ok()
        .map(|b| b.trim().to_owned())
        .filter(|b| !b.is_empty() && !b.starts_with(aep::project::branches::STAGE_PREFIX))
    else {
        return Ok(None);
    };
    let base_sha = aep::workspace::commit(&root, &format!("refs/heads/{base}"))?;
    let contained = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["merge-base", "--is-ancestor", &base_sha, "HEAD"])
        .status()?
        .success();
    if contained {
        return Ok(None);
    }
    let behind: u64 = git(cwd, &["rev-list", "--count", &format!("HEAD..{base_sha}")])?
        .trim()
        .parse()
        .unwrap_or(0);
    let tree = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args([
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            "HEAD",
            &base_sha,
        ])
        .output()?;
    let conflicts: Vec<String> = match tree.status.code() {
        Some(0) => vec![],
        Some(1) => {
            let mut files: Vec<String> = String::from_utf8_lossy(&tree.stdout)
                .lines()
                .skip(1)
                .take_while(|l| !l.is_empty())
                .map(str::to_owned)
                .collect();
            files.dedup();
            files
        }
        _ => anyhow::bail!(
            "无法检查事项分支能否合入 {base}：{}",
            String::from_utf8_lossy(&tree.stderr).trim()
        ),
    };
    let dirty = !git(cwd, &["status", "--porcelain", "--untracked-files=no"])?
        .trim()
        .is_empty();
    record["base"] = json!({"branch":base,"sha":base_sha,"behind":behind,"conflicts":conflicts});
    let listed = conflicts
        .iter()
        .take(12)
        .cloned()
        .collect::<Vec<_>>()
        .join("、");
    let stages = &req.flow().stages;
    let before = |role: Role| (0..req.stage).rev().find(|&i| stages[i].role == role);
    let back = |record: &mut Value, to: usize, question: String| -> Result<Option<String>> {
        record["route_back"] = json!({"stage":to,"name":stages[to].name,"question":question});
        anyhow::bail!("需要「{}」先解决：{question}", stages[to].name)
    };
    match stage.role {
        Role::Plan | Role::Build | Role::Test if conflicts.is_empty() && !dirty => {
            let message = format!("合入 {base}（{behind} 个新提交）\n\nAEP-Run: {id}");
            let mut args = identity(cwd);
            args.extend([
                "merge",
                "--no-edit",
                "--no-verify",
                "-q",
                "-m",
                &message,
                &base_sha,
            ]);
            git(cwd, &args)?;
            record["base"]["merged"] = json!(true);
            Ok(Some(format!(
                "平台已把 {base} 的 {behind} 个新提交合入事项分支（没有冲突），本阶段基于合并后的代码。"
            )))
        }
        Role::Plan | Role::Build => Ok(Some(if conflicts.is_empty() {
            format!(
                "事项分支落后 {base} {behind} 个提交，工作区有未提交改动，平台没有自动合并。完成本阶段改动前请运行 git merge {base} 并处理冲突（不要提交，平台会自动提交）。"
            )
        } else {
            format!(
                "事项分支落后 {base} {behind} 个提交，合并会在这些文件冲突：{listed}。请先运行 git merge {base}，解决冲突并让测试通过，再继续本阶段（不要提交，平台会把合并一起提交）。"
            )
        })),
        Role::Test => match before(Role::Build) {
            Some(b) if !conflicts.is_empty() => back(
                record,
                b,
                format!(
                    "{base} 前进了 {behind} 个提交，合入时这些文件冲突：{listed}。请合入 {base} 并解决冲突。"
                ),
            ),
            _ => Ok(None),
        },
        Role::Accept | Role::Deliver => {
            let target = if conflicts.is_empty() {
                before(Role::Test).or(before(Role::Build))
            } else {
                before(Role::Build)
            };
            match target {
                Some(t) => back(
                    record,
                    t,
                    if conflicts.is_empty() {
                        format!(
                            "{base} 在测试之后又前进了 {behind} 个提交，已验证的提交无法快进合入。请在合入 {base} 后的代码上重新验证。"
                        )
                    } else {
                        format!(
                            "{base} 在测试之后又前进了 {behind} 个提交，合入时这些文件冲突：{listed}。请合入 {base} 并解决冲突。"
                        )
                    },
                ),
                None => Ok(None),
            }
        }
    }
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

/// Subtask paths in the plan's `task.json` files that point nowhere: neither the path nor
/// its parent directory exists in the worktree (a typo, or a guessed module layout).
fn invalid_paths(cwd: &Path, specs: &[String]) -> Vec<String> {
    let mut bad = Vec::new();
    for file in specs.iter().filter(|f| f.ends_with("task.json")) {
        let Some(plan) = fs::read(cwd.join(file))
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        else {
            continue;
        };
        let tasks: Vec<aep::workflow::Subtask> =
            serde_json::from_value(plan["subtasks"].clone()).unwrap_or_default();
        for task in tasks {
            for raw in &task.paths {
                // Globs are checked up to their first wildcard; absolute paths inside the
                // worktree are fine, anything escaping it is not.
                let literal = raw.split(['*', '?', '[']).next().unwrap_or_default();
                let literal = literal.trim().trim_end_matches('/');
                let path = Path::new(literal);
                let path = path.strip_prefix(cwd).unwrap_or(path);
                let escapes = path.is_absolute()
                    || path
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir));
                let full = cwd.join(path);
                let found = literal.is_empty()
                    || full.exists()
                    || full.parent().is_some_and(|p| p.is_dir());
                if escapes || !found {
                    bad.push(format!("{} {raw}", task.id));
                }
            }
        }
    }
    bad
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
        // Checked once the session is recorded, so the planner can be asked to fix them.
        let invalid = invalid_paths(cwd, &specs);
        if !invalid.is_empty() {
            record["invalid_paths"] = json!(invalid);
        }
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
    /// The newest round whose session was saved; its evidence decides whether to resume.
    session_run: Option<String>,
    result: String,
}
/// Load what a follow-up continues: the latest workspace, the newest session that was
/// actually saved, and the newest non-empty answer. A failed round (for example a resume
/// that could not find its session) defers to the round it continued.
fn prior_turn(
    dir: &Path,
    follow: &aep::workflow::FollowUp,
    repository: Option<&Path>,
) -> Result<Prior> {
    let runs = dir.parent().context("missing runs directory")?;
    let mut id = follow.attempt.clone();
    let mut workspace = None;
    let mut session = None;
    let mut session_run = None;
    let mut result = String::new();
    for _ in 0..64 {
        aep::workspace::ident(&id, 64)?;
        let previous = runs.join(&id);
        let ev = aep::workflow::evidence(&previous)?;
        if workspace.is_none() {
            workspace = ev["workspace"].as_str().map(PathBuf::from);
        }
        // Runs from before sessions were persisted (`--ephemeral`) cannot be resumed.
        if session.is_none() {
            session = if ev["resumable"] == true {
                ev["session"].as_str().map(str::to_owned)
            } else {
                zcode_app_session(&ev)
            };
            if session.is_some() {
                session_run = Some(id.clone());
            }
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
    let mut workspace = workspace.context("上一轮执行没有工作区")?;
    // A stage worktree moved out of `.git` since that turn is found again by its branch.
    if !workspace.is_dir()
        && let (Some(repo), Some(name)) = (
            repository.and_then(|r| Workspace::open(r).ok()),
            workspace
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_owned),
        )
        && let Some((_, moved)) = aep::project::branches::stage_worktrees(&repo.root)
            .into_iter()
            .find(|(checkout, p)| *checkout == name && p.is_dir())
    {
        workspace = moved;
    }
    ensure!(workspace.is_dir(), "上一轮执行的工作区已移除，请重新执行");
    Ok(Prior {
        workspace,
        session_run,
        result,
    })
}

/// The ZCode app-server session a run saved (`native_session`); desktop tasks have none.
fn zcode_app_session(ev: &Value) -> Option<String> {
    let native = &ev["native_session"];
    (native["client"] == "ZCode" && native["transport"] == "app-server")
        .then(|| native["session"].as_str().map(str::to_owned))
        .flatten()
        .filter(|s| !s.trim().is_empty())
}

/// A ZCode follow-up continues the newest app-server session of the rounds it follows.
fn zcode_continue(runs: &Path, stage: usize, prior: &Prior) -> Plan {
    let found = prior.session_run.as_ref().and_then(|run| {
        let ev = aep::workflow::evidence(&runs.join(run)).ok()?;
        Some((run.clone(), zcode_app_session(&ev)?))
    });
    match found {
        Some((run, session)) => Plan::Resume {
            session,
            fork: false,
            source_stage: stage,
            source_run: run,
            context: None,
        },
        None => Plan::Fresh {
            reason: Reason::SessionUnavailable,
            context: None,
        },
    }
}

fn first_event(stdout: &Path, kind: &str) -> Option<Value> {
    BufReader::new(fs::File::open(stdout).ok()?)
        .lines()
        .take(40)
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str::<Value>(&l).ok())
        .find(|v| v["type"] == kind && (v["session_id"].is_string() || v["thread_id"].is_string()))
}

/// Watch the CLI's own token counters while it runs and stop it once it has used `limit`.
fn watch_budget(
    dir: &Path,
    work: &Path,
    id: &str,
    limit: u64,
) -> (
    std::sync::Arc<std::sync::atomic::AtomicBool>,
    std::thread::JoinHandle<()>,
) {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let done = Arc::new(AtomicBool::new(false));
    let flag = done.clone();
    let (dir, work, id) = (dir.to_owned(), work.to_owned(), id.to_owned());
    let handle = std::thread::spawn(move || {
        let mut warned = false;
        while !flag.load(Ordering::Relaxed) {
            for _ in 0..10 {
                if flag.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            let Ok(report) = aep::usage::measure(&dir) else {
                continue;
            };
            let used = report.summary.tokens.total();
            if !warned && used * 10 >= limit * 8 {
                warned = true;
                let _ = aep::project::save_json(
                    &dir,
                    "budget-warning.json",
                    &json!({"limit": limit, "used": used}),
                );
            }
            if used >= limit {
                let _ = aep::project::save_json(
                    &dir,
                    "budget.json",
                    &json!({"limit": limit, "used": used, "at": chrono::Utc::now().to_rfc3339()}),
                );
                if let Ok(stop) = Cancellation::new(&work, &id) {
                    let _ = stop.request();
                }
                return;
            }
        }
    });
    (done, handle)
}

/// The CLI could not find the session it was asked to resume (pruned or never saved).
fn session_missing(dir: &Path) -> bool {
    ["stderr.log", "stdout.log"].iter().any(|name| {
        let log = tail(&dir.join(name), 4000).to_lowercase();
        log.contains("no rollout found")
            || log.contains("no conversation found")
            || log.contains("thread/resume failed")
            || log.contains(crate::zcode_stage::RESUME_FAILED)
    })
}

/// Adds what the Claude run actually loaded (init event, first call's context) to `launch`.
fn claude_launch_stats(client: Client, stdout: &Path, record: &mut Value) {
    if client != Client::Claude || !record["launch"].is_object() {
        return;
    }
    let stats = fs::read_to_string(stdout)
        .ok()
        .and_then(|raw| aep::launch::claude_init_stats(&raw));
    record["launch"] = aep::launch::with_stats(record["launch"].take(), stats);
}

/// This run's own calls and the context of its last call, read by the next run deciding
/// whether to continue the session (R2) and shown as the stage's window usage (AC-8).
fn usage_window(client: Client, dir: &Path, record: &mut Value) {
    if !matches!(client, Client::Codex | Client::Claude) {
        return;
    }
    if let Ok(report) = aep::usage::measure(dir) {
        record["usage_window"] = json!({"calls": report.summary.calls});
        if let Some(c) = report.summary.last_context {
            record["last_context"] = json!(c);
        }
    }
}

/// Codex's last reported usage, cumulative over the thread after `exec resume`: the next
/// resume of this session subtracts it when the rollout cannot be read.
fn codex_session_total(client: Client, stdout: &Path, record: &mut Value) {
    if client != Client::Codex {
        return;
    }
    if let Some(total) = fs::read_to_string(stdout)
        .ok()
        .and_then(|raw| aep::usage::codex_session_total(&raw))
    {
        record["codex_session_total"] = total;
    }
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

/// Returns the refused file writes and the evidence for every permission denial.
fn denials(v: &Value) -> (Vec<String>, Vec<Value>) {
    let mut refused = Vec::new();
    let mut all = Vec::new();
    for denial in v["permission_denials"].as_array().into_iter().flatten() {
        let tool = denial["tool_name"].as_str();
        let input = &denial["tool_input"];
        let path = input["file_path"]
            .as_str()
            .or_else(|| input["notebook_path"].as_str());
        if let Some(tool @ ("Edit" | "Write" | "MultiEdit" | "NotebookEdit")) = tool {
            refused.push(
                path.map(str::to_owned)
                    .unwrap_or_else(|| format!("{tool}（无路径）")),
            );
        }
        let target = path
            .or_else(|| input["command"].as_str())
            .or_else(|| input["path"].as_str());
        all.push(json!({"tool": tool, "target": target}));
    }
    (refused, all)
}

fn record_claude_denials(v: &Value, write: bool, record: &mut Value) -> Result<()> {
    let (refused, all) = denials(v);
    record["denied_bash"] = json!(all.iter().filter(|d| d["tool"] == "Bash").count());
    record["permission_denials"] = json!(all);
    ensure!(
        !write || refused.is_empty(),
        "Claude 的文件写入被拒绝 {} 次，本阶段没有完成：{}",
        refused.len(),
        refused.join("，")
    );
    Ok(())
}

fn claude_stage_args(
    args: &mut Vec<String>,
    dir: &Path,
    receipt: Option<&Path>,
    sandbox: Sandbox,
    write: bool,
) {
    if let Some(parent) = receipt.and_then(Path::parent) {
        args.extend(["--add-dir".into(), parent.display().to_string()]);
    }
    args.extend(["--add-dir".into(), dir.display().to_string()]);
    // The hand-off points at earlier runs. Claude's absolute Read rules start with "//".
    if let Some(runs) = dir.parent() {
        args.extend([
            "--allowedTools".into(),
            format!("Read(/{}/**)", runs.display()),
        ]);
    }
    args.extend(
        [
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--permission-mode",
            match sandbox {
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
    if sandbox == Sandbox::Network {
        // Print mode cannot ask for approval; let the stage run its test commands.
        args.extend(["--allowedTools".into(), "Bash".into()]);
    }
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
    resume: Option<(String, bool)>,
    gate: bool,
    write: bool,
    (project_ids, repo_ids): (Vec<i64>, Vec<i64>),
) -> Result<()> {
    let cwd = cwd.to_path_buf();
    // What happened with the base branch before this stage started (`align_base`).
    let prompt = match record["base"]["note"].as_str() {
        Some(note) => format!("{prompt}\n\n{note}"),
        None => prompt,
    };
    let (resume, fork) = match resume {
        Some((session, fork)) => (Some(session), fork),
        None => (None, false),
    };
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
            claude_stage_args(&mut args, dir, receipt.as_deref(), req.sandbox, write);
            // No user MCP, settings, skills or auto memory: the stage starts from the project.
            let launch = aep::launch::claude_minimal(&bin, &profile.model);
            args.extend(launch.args.iter().cloned());
            record["launch"] = launch.evidence();
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
            if fork {
                // A later stage gets its own session ID; the earlier one stays as it was.
                args.push("--fork-session".into());
            }
        } else if profile.client == Client::ZCode {
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
    let watch = req
        .token_limit
        .map(|limit| watch_budget(dir, work, id, limit));
    let code = logged(
        &args,
        &cwd,
        &stdout,
        // Stage sessions run until they finish, the user cancels them or the budget runs out.
        u64::MAX,
        (profile.client != Client::ZCode).then_some(prompt.as_str()),
        Some(stderr.clone()),
        stop,
    );
    if let Some((done, handle)) = watch {
        done.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = handle.join();
    }
    usage_window(profile.client, dir, record);
    codex_session_total(profile.client, &stdout, record);
    let code = match code {
        Ok(code) => code,
        Err(e) => {
            claude_launch_stats(profile.client, &stdout, record);
            if dir.join("budget.json").is_file() {
                // Stopped for its budget: keep the session so a follow-up can continue it.
                record["workspace"] = json!(cwd);
                record["resumable"] =
                    json!(matches!(profile.client, Client::Codex | Client::Claude));
                record["session"] = match profile.client {
                    Client::Claude => first_event(&stdout, "system")
                        .map(|v| v["session_id"].clone())
                        .unwrap_or(Value::Null),
                    Client::Codex => first_event(&stdout, "thread.started")
                        .map(|v| v["thread_id"].clone())
                        .unwrap_or(Value::Null),
                    _ => Value::Null,
                };
                if record["session"].is_null()
                    && let Some(session) = &resume
                {
                    record["session"] = json!(session);
                }
                // A ZCode app-server session saved before the stop can be continued (R7).
                if profile.client == Client::ZCode
                    && let Ok(native) = aep::workspace::read(&dir.join("session.json"))
                {
                    record["native_session"] = native;
                }
            }
            return Err(e);
        }
    };
    record["exit_code"] = json!(code);
    claude_launch_stats(profile.client, &stdout, record);
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

    // A result event can carry denials even when Claude exits with an error.
    let claude = (profile.client == Client::Claude).then(|| claude_result(&stdout));
    if let Some(Ok(v)) = &claude {
        if !v["session_id"].is_null() {
            record["session"] = v["session_id"].clone();
        }
        if let Some(usage) = v["modelUsage"].as_object() {
            record["reported_models"] = json!(usage.keys().collect::<Vec<_>>());
        }
        record_claude_denials(v, write, record)?;
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
    let result = match claude {
        Some(result) => {
            let v = result?;
            let s = v["result"].as_str().context("Claude 未生成最终答复")?;
            ensure!(v["is_error"] != true, "Claude: {s}");
            s.into()
        }
        None if profile.client == Client::Codex => {
            fs::read_to_string(&result_path).context("Codex 未生成最终答复")?
        }
        None => fs::read_to_string(stdout)?,
    };
    ensure!(!result.trim().is_empty(), "CLI 返回空结果");
    fs::write(&result_path, &result)?;
    let flow = req.flow();
    let skips = flow.skips(req.stage, &result);
    if !skips.is_empty() {
        record["skip"] = json!(
            skips
                .iter()
                .map(|(s, r)| json!({"stage":s,"name":flow.stages[*s].name,"reason":r}))
                .collect::<Vec<_>>()
        );
    }
    if let Some(invalid) = record["invalid_paths"].as_array().filter(|a| !a.is_empty()) {
        anyhow::bail!(
            "task.json 里有子任务路径在仓库中找不到：{}。请改成仓库里真实存在的文件或目录；要新建的，写到父目录已存在的那一级（例如新目录本身）",
            invalid
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("、")
        );
    }
    match flow.route_back(req.stage, &result) {
        Back::To {
            stage: to,
            question,
        } => {
            let target = &flow.stages[to].name;
            record["route_back"] = json!({"stage":to,"name":target,"question":question});
            anyhow::bail!("需要「{target}」先解决：{question}");
        }
        Back::Rejected {
            name,
            question,
            allowed,
        } => {
            record["route_back_rejected"] =
                json!({"name":name,"question":question,"allowed":allowed});
        }
        Back::None => {}
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
    if !stage_worktree(&repo.root, cwd) {
        return Ok(());
    }
    let status = |cwd: &Path| git(cwd, &["status", "--porcelain", "--untracked-files=all"]);
    if matches!(stage.role, Role::Plan | Role::Build) && !status(cwd)?.trim().is_empty() {
        git(cwd, &["add", "-A"])?;
        let title = match &req.subtask {
            Some(t) if t.title.trim().is_empty() => format!("{} · {}", stage.name, t.id),
            Some(t) => format!("{} · {} {}", stage.name, t.id, t.title.trim()),
            None => stage.name.to_owned(),
        };
        let message = format!("{} · {title}\n\nAEP-Run: {id}", req.work);
        let mut args = identity(cwd);
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
        .flow()
        .stages
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
        let budget = aep::workspace::read(&dir.join("budget.json")).ok();
        record["status"] = json!(if budget.is_some() {
            "budget_exhausted"
        } else if stop.requested()? {
            "cancelled"
        } else {
            "failed"
        });
        record["error"] = json!(match &budget {
            Some(b) => format!(
                "Token 预算已用完：这一轮用了 {}，本轮可用 {}。追加预算后可在原会话继续",
                aep::usage::compact(b["used"].as_u64().unwrap_or(0)),
                aep::usage::compact(b["limit"].as_u64().unwrap_or(0))
            ),
            None => format!("{e:#}"),
        });
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

    /// Runs `<root>/runs/<id>/evidence.json`; returns the directory of a new round.
    fn zcode_rounds(root: &Path, rounds: &[(&str, Value)]) -> PathBuf {
        for (id, ev) in rounds {
            let d = root.join("runs").join(id);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("evidence.json"), ev.to_string()).unwrap();
            fs::write(d.join("result.txt"), format!("answer of {id}")).unwrap();
        }
        let next = root.join("runs").join("r3");
        fs::create_dir_all(&next).unwrap();
        next
    }

    /// A repository on `main` with an item branch checked out as a stage worktree.
    struct Base {
        _t: tempfile::TempDir,
        root: PathBuf,
        wt: PathBuf,
    }
    fn base_fixture() -> Base {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("repo");
        fs::create_dir(&root).unwrap();
        let g = |dir: &Path, args: &[&str]| git(dir, args).unwrap();
        g(&root, &["init", "-q", "-b", "main"]);
        g(&root, &["config", "user.email", "t@example.com"]);
        g(&root, &["config", "user.name", "t"]);
        fs::write(root.join("a.txt"), "one\n").unwrap();
        fs::write(root.join("b.txt"), "one\n").unwrap();
        g(&root, &["add", "-A"]);
        g(&root, &["commit", "-q", "-m", "base"]);
        let wt = t.path().join("wt");
        g(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "aep-stage/item",
                wt.to_str().unwrap(),
            ],
        );
        fs::write(wt.join("a.txt"), "item\n").unwrap();
        g(&wt, &["commit", "-q", "-am", "item"]);
        Base { _t: t, root, wt }
    }
    fn advance(b: &Base, file: &str) {
        fs::write(b.root.join(file), "main\n").unwrap();
        git(&b.root, &["commit", "-q", "-am", "main moved"]).unwrap();
    }
    fn base_request(b: &Base, stage: usize) -> Request {
        serde_json::from_value(json!({
            "project":"p","work":"w","template":"Software","stage":stage,
            "profile":{"client":"Claude","executable":"","model":"","check_command":""},
            "repository":b.root,"brief":"","approved":[]
        }))
        .unwrap()
    }
    fn align(b: &Base, stage: usize) -> (Result<Option<String>>, Value) {
        let req = base_request(b, stage);
        let st = &req.flow().stages[stage];
        let mut record = json!({});
        let r = align_base(&req, st, &b.wt, "run1", &mut record);
        (r, record)
    }

    #[test]
    fn base_unchanged_does_nothing() {
        let b = base_fixture();
        let (r, record) = align(&b, 3);
        assert_eq!(r.unwrap(), None);
        assert!(record["base"].is_null());
    }

    #[test]
    fn base_moved_cleanly_is_merged_before_the_build_and_the_test() {
        for stage in [3, 4] {
            let b = base_fixture();
            advance(&b, "b.txt");
            let (r, record) = align(&b, stage);
            let note = r.unwrap().expect("a note for the prompt");
            assert!(note.contains("main 的 1 个新提交"), "{note}");
            assert_eq!(record["base"]["merged"], true, "{record}");
            assert_eq!(fs::read_to_string(b.wt.join("b.txt")).unwrap(), "main\n");
            assert_eq!(fs::read_to_string(b.wt.join("a.txt")).unwrap(), "item\n");
            let main = aep::workspace::commit(&b.root, "main").unwrap();
            assert!(git(&b.wt, &["merge-base", "--is-ancestor", &main, "HEAD"]).is_ok());
            assert!(
                git(&b.wt, &["log", "-1", "--format=%B"])
                    .unwrap()
                    .contains("AEP-Run: run1")
            );
            // Already contained now: the next stage has nothing to do.
            assert_eq!(align(&b, stage).0.unwrap(), None);
        }
    }

    #[test]
    fn base_conflicts_are_handed_to_the_build_and_send_the_test_back() {
        let b = base_fixture();
        advance(&b, "a.txt");
        let head = aep::workspace::commit(&b.wt, "HEAD").unwrap();
        let (r, record) = align(&b, 3);
        let note = r.unwrap().unwrap();
        assert!(
            note.contains("a.txt") && note.contains("git merge main"),
            "{note}"
        );
        assert_eq!(record["base"]["conflicts"], json!(["a.txt"]));
        assert_eq!(
            aep::workspace::commit(&b.wt, "HEAD").unwrap(),
            head,
            "nothing merged"
        );
        // The test stage cannot resolve conflicts: back to the build, no model call.
        let (r, record) = align(&b, 4);
        assert!(r.is_err());
        assert_eq!(record["route_back"]["stage"], 3, "{record}");
        assert!(
            record["route_back"]["question"]
                .as_str()
                .unwrap()
                .contains("a.txt")
        );
    }

    #[test]
    fn base_moved_after_testing_sends_acceptance_and_delivery_back() {
        let b = base_fixture();
        advance(&b, "b.txt");
        let head = aep::workspace::commit(&b.wt, "HEAD").unwrap();
        for stage in [5, 6] {
            let (r, record) = align(&b, stage);
            assert!(r.is_err());
            // Merges cleanly: re-test on the merged code; the checked commit is untouched.
            assert_eq!(record["route_back"]["stage"], 4, "{record}");
            assert_eq!(aep::workspace::commit(&b.wt, "HEAD").unwrap(), head);
        }
        advance(&b, "a.txt");
        let (r, record) = align(&b, 5);
        assert!(r.is_err());
        assert_eq!(
            record["route_back"]["stage"], 3,
            "conflicts go to the build: {record}"
        );
    }

    #[test]
    fn zcode_stage_follow_up_continues_the_saved_app_server_session() {
        let t = tempfile::tempdir().unwrap();
        let ws = t.path().join("ws");
        fs::create_dir(&ws).unwrap();
        let native = json!({"client":"ZCode","transport":"app-server","session":"sess_a"});
        // r2 (budget-stopped or failed without a session) follows r1, which saved one.
        let dir = zcode_rounds(
            t.path(),
            &[
                (
                    "r1",
                    json!({"status":"succeeded","workspace":ws,"resumable":false,"native_session":native}),
                ),
                (
                    "r2",
                    json!({"status":"failed","workspace":ws,"resumable":false,"follow_up":{"attempt":"r1"}}),
                ),
            ],
        );
        let follow = aep::workflow::FollowUp {
            attempt: "r2".into(),
            message: "继续".into(),
        };
        let prior = prior_turn(&dir, &follow, None).unwrap();
        assert_eq!(prior.session_run.as_deref(), Some("r1"));
        match zcode_continue(&t.path().join("runs"), 3, &prior) {
            Plan::Resume {
                session,
                fork,
                source_run,
                ..
            } => {
                assert_eq!(session, "sess_a");
                assert!(!fork);
                assert_eq!(source_run, "r1");
            }
            other => panic!("expected a resume, got {other:?}"),
        }
    }

    #[test]
    fn zcode_stage_desktop_tasks_are_not_continued() {
        let t = tempfile::tempdir().unwrap();
        let ws = t.path().join("ws");
        fs::create_dir(&ws).unwrap();
        let native = json!({"client":"ZCode","transport":"desktop","session":"task_1"});
        let dir = zcode_rounds(
            t.path(),
            &[(
                "r1",
                json!({"status":"succeeded","workspace":ws,"native_session":native}),
            )],
        );
        let follow = aep::workflow::FollowUp {
            attempt: "r1".into(),
            message: "继续".into(),
        };
        let prior = prior_turn(&dir, &follow, None).unwrap();
        assert_eq!(prior.session_run, None);
        assert!(matches!(
            zcode_continue(&t.path().join("runs"), 3, &prior),
            Plan::Fresh {
                reason: Reason::SessionUnavailable,
                ..
            }
        ));
    }

    #[test]
    fn zcode_stage_resume_failure_falls_back_to_full_context() {
        let t = tempfile::tempdir().unwrap();
        assert!(!session_missing(t.path()));
        fs::write(
            t.path().join("stderr.log"),
            format!(
                "{}: session not found\nError: ZCode\n",
                crate::zcode_stage::RESUME_FAILED
            ),
        )
        .unwrap();
        assert!(session_missing(t.path()));
        let mut record = json!({"session":"sess_a","reused_session":{"session":"sess_a"}});
        resume_failed(&mut record, &anyhow::anyhow!("resume failed"));
        assert_eq!(record["fresh_session_reason"], "session_unavailable");
        assert!(record["session"].is_null());
    }

    #[test]
    fn claude_denials_fail_write_stages_and_keep_evidence() {
        let result = json!({"permission_denials": [
            {"tool_name": "Write", "tool_input": {"file_path": "/tmp/probe/x.md"}},
            {"tool_name": "Bash", "tool_input": {"command": "mkdir /tmp/probe"}},
        ]});
        let mut record = json!({});
        let error = record_claude_denials(&result, true, &mut record).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Claude 的文件写入被拒绝 1 次，本阶段没有完成：/tmp/probe/x.md"
        );
        let expected = json!({
            "permission_denials": [
                {"tool": "Write", "target": "/tmp/probe/x.md"},
                {"tool": "Bash", "target": "mkdir /tmp/probe"},
            ],
            "denied_bash": 1,
        });
        assert_eq!(record, expected);

        let mut read_only = json!({});
        record_claude_denials(&result, false, &mut read_only).unwrap();
        assert_eq!(read_only, expected);
    }

    #[test]
    fn claude_denials_include_notebooks_and_writes_without_paths() {
        let result = json!({"permission_denials": [
            {"tool_name": "NotebookEdit", "tool_input": {"notebook_path": "/tmp/probe.ipynb"}},
            {"tool_name": "Edit", "tool_input": {"file_path": "/tmp/edit", "notebook_path": "/tmp/other"}},
            {"tool_name": "MultiEdit", "tool_input": {"file_path": null}},
            {"tool_name": "Write"},
        ]});
        let (refused, all) = denials(&result);
        assert_eq!(
            refused,
            [
                "/tmp/probe.ipynb",
                "/tmp/edit",
                "MultiEdit（无路径）",
                "Write（无路径）"
            ]
        );
        assert_eq!(
            all,
            [
                json!({"tool": "NotebookEdit", "target": "/tmp/probe.ipynb"}),
                json!({"tool": "Edit", "target": "/tmp/edit"}),
                json!({"tool": "MultiEdit", "target": null}),
                json!({"tool": "Write", "target": null}),
            ]
        );
    }

    #[test]
    fn claude_denials_record_bash_and_other_tools_without_failing() {
        let result = json!({"permission_denials": [
            {"tool_name": "Bash", "tool_input": {"command": "ls /tmp", "path": "/other"}},
            {"tool_name": "Read", "tool_input": {"path": "/tmp/probe"}},
            {"tool_name": "Bash"},
        ]});
        let mut record = json!({});
        record_claude_denials(&result, true, &mut record).unwrap();
        assert_eq!(record["denied_bash"], 2);
        assert_eq!(
            record["permission_denials"],
            json!([
                {"tool": "Bash", "target": "ls /tmp"},
                {"tool": "Read", "target": "/tmp/probe"},
                {"tool": "Bash", "target": null},
            ])
        );
    }

    #[test]
    fn claude_denials_allow_missing_or_null_denials() {
        for result in [json!({}), json!({"permission_denials": null})] {
            let mut record = json!({});
            record_claude_denials(&result, true, &mut record).unwrap();
            assert_eq!(record, json!({"permission_denials": [], "denied_bash": 0}));
        }
    }

    #[test]
    fn claude_allowed_tools_scopes_read_to_runs_with_spaces() {
        let dir = Path::new("/home/user/Application Support/AEP Studio/runs/current");
        let receipt = dir.join("inputs/source-evidence.json");
        let mut args = Vec::new();
        claude_stage_args(&mut args, dir, Some(&receipt), Sandbox::Standard, true);

        assert_eq!(
            &args[..6],
            [
                "--add-dir",
                "/home/user/Application Support/AEP Studio/runs/current/inputs",
                "--add-dir",
                "/home/user/Application Support/AEP Studio/runs/current",
                "--allowedTools",
                "Read(//home/user/Application Support/AEP Studio/runs/**)",
            ]
        );
        assert!(!args[6..].iter().any(|arg| arg == "--add-dir"));
    }

    #[test]
    fn claude_allowed_tools_keeps_network_bash_authorization() {
        let dir = Path::new("/work/runs/current");
        let mut args = Vec::new();
        claude_stage_args(&mut args, dir, None, Sandbox::Network, false);

        let allowed: Vec<_> = args
            .windows(2)
            .filter(|pair| pair[0] == "--allowedTools")
            .map(|pair| pair[1].as_str())
            .collect();
        assert_eq!(allowed, ["Read(//work/runs/**)", "Bash"]);
    }

    #[test]
    fn launch_stats_are_added_to_claude_evidence_only() {
        let t = tempfile::tempdir().unwrap();
        let out = t.path().join("stdout.log");
        fs::write(
            &out,
            concat!(
                r#"{"type":"system","subtype":"init","tools":["Read","Bash"],"mcp_servers":[],"skills":[],"plugins":[{"name":"agents-md","source":"agents-md@builtin"}],"claude_code_version":"9"}"#,
                "\n",
                r#"{"type":"assistant","message":{"usage":{"input_tokens":5,"cache_creation_input_tokens":7000}}}"#,
                "\n"
            ),
        )
        .unwrap();
        let launch = aep::launch::compose(Err("x"), "m", None);
        let mut record = json!({"launch": launch.evidence()});
        claude_launch_stats(Client::Claude, &out, &mut record);
        let l = &record["launch"];
        assert_eq!(l["diet"], aep::launch::DIET);
        assert_eq!(l["mcp_servers"], 0);
        assert_eq!(l["skills"], 0);
        assert_eq!(l["tools"], 2);
        assert_eq!(l["builtin_plugins"], json!(["agents-md"]));
        assert_eq!(l["first_context"], 7005);
        let mut codex = json!({});
        claude_launch_stats(Client::Codex, &out, &mut codex);
        assert!(codex["launch"].is_null());
    }

    #[test]
    fn codex_records_its_last_reported_session_total() {
        let t = tempfile::tempdir().unwrap();
        let out = t.path().join("stdout.log");
        fs::write(
            &out,
            concat!(
                r#"{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":1}}"#,
                "\n",
                r#"{"type":"turn.completed","usage":{"input_tokens":30,"output_tokens":2}}"#,
                "\n"
            ),
        )
        .unwrap();
        let mut record = json!({});
        codex_session_total(Client::Codex, &out, &mut record);
        assert_eq!(record["codex_session_total"]["input_tokens"], 30);
        let mut claude = json!({});
        codex_session_total(Client::Claude, &out, &mut claude);
        assert!(claude["codex_session_total"].is_null());
    }

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
