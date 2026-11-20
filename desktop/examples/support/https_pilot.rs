use super::{Args, Driver, inspect};
use aep::workspace::{Workspace, git, read};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    collections::HashSet,
    fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

pub(super) fn sessions(parent: &Path) -> HashSet<PathBuf> {
    fs::read_dir(parent)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|ty| ty.is_dir()))
        .map(|entry| entry.path())
        .collect()
}

pub(super) fn await_stage(
    ui: &mut Driver<'_>,
    label: &str,
    session: &Path,
    seconds: u64,
) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        ui.cx.advance_clock(Duration::from_secs(1));
        ui.settle()?;
        let state = read(&session.join("state.json")).unwrap_or_default();
        ensure!(
            state["exit_code"].as_i64().is_none_or(|code| code == 0)
                && ui.absent("https-failure")?,
            "Studio pilot failed in {}. See {}",
            state["stage"],
            session.join("studio.log").display()
        );
        if ui
            .element("https-stage")
            .is_ok_and(|element| element.label() == Some(label))
        {
            return Ok(state);
        }
        ensure!(Instant::now() < deadline, "timed out waiting for {label}");
        thread::sleep(Duration::from_millis(200));
    }
}

pub(super) fn confirm(ui: &mut Driver<'_>, action: &str, checkpoint: &str) -> Result<()> {
    ensure!(
        ui.absent("https-confirm")?,
        "Studio exposed an unconfirmed action"
    );
    print!("Type {action} to approve this Studio action: ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    ensure!(answer.trim() == action, "operator declined {action}");
    ui.reveal("https-pilot-page", "https-ack")?;
    ui.click("https-ack")?;
    ui.reveal("https-pilot-page", "https-confirm")?;
    ui.checkpoint(checkpoint)?;
    ui.click("https-confirm")
}

pub(super) fn run(mut args: Args) -> Result<()> {
    ensure!(
        io::stdin().is_terminal(),
        "--live-https-pilot requires an operator terminal"
    );
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize()?;
    let ws = Workspace::open(&source)?;
    let parent = ws.data.join("https-studio");
    let before = sessions(&parent);
    let output = args
        .output_dir
        .as_ref()
        .context("missing checkpoint directory")?;
    fs::create_dir_all(output)?;
    let output = tempfile::Builder::new()
        .prefix("studio-https-")
        .tempdir_in(output.canonicalize()?)?
        .keep();
    args.output_dir = Some(output.clone());
    args.screenshots = true;
    println!("Studio checkpoints: {}", output.display());
    inspect(&args, &ws, |ui| {
        ui.click("HTTPS 试点")?;
        ui.checkpoint("01-studio-entry")?;
        ui.click("https-start")?;
        let created = sessions(&parent);
        let new: Vec<_> = created.difference(&before).collect();
        ensure!(
            new.len() == 1,
            "Studio did not create exactly one pilot session"
        );
        let session = new[0];
        println!("Pilot session: {}", session.display());

        await_stage(ui, "HTTPS 入口已暂停，等待 Lease 过期", session, 900)?;
        ui.checkpoint("02-https-outage")?;
        await_stage(ui, "对同一候选重新验收，运行 Claude 审查", session, 90)?;
        ui.checkpoint("03-recovery")?;

        let pending = await_stage(ui, "待人工确认发布", session, 780)?;
        let task = pending["task"].as_str().context("missing task")?;
        let old = pending["old_run"].as_str().context("missing first Run")?;
        let recovered = pending["recovered_run"]
            .as_str()
            .context("missing recovery Run")?;
        let candidate = pending["candidate"].as_str().context("missing candidate")?;
        let dir = session.join(task);
        let evidence = dir.join("evidence");
        let first = read(&evidence.join("worker/runs").join(old).join("evidence.json"))?;
        let next = read(
            &evidence
                .join("worker/runs")
                .join(recovered)
                .join("evidence.json"),
        )?;
        ensure!(
            first["candidate_sha"] == candidate
                && next["candidate_sha"] == candidate
                && first["status"] == "cancelled"
                && next["check_exit_code"] == 0
                && first["usage"]["implementer"]["output_tokens"]
                    .as_u64()
                    .is_some_and(|n| n > 0)
                && next["usage"]["reviewer"]["output_tokens"]
                    .as_u64()
                    .is_some_and(|n| n > 0)
                && next["blocking_findings"]
                    .as_array()
                    .is_some_and(Vec::is_empty),
            "the recovery did not use the same real candidate and reviewer"
        );
        ensure!(
            !evidence.join("publication.json").exists(),
            "published before confirmation"
        );
        println!(
            "Read candidate.diff and the recovered checks.log and review.txt in {}",
            evidence.display()
        );
        ui.checkpoint("04-publication-review")?;
        confirm(
            ui,
            &format!("publish {}", &candidate[..12]),
            "05-publication-confirmed",
        )?;

        await_stage(ui, "等待并核对 GitHub CI", session, 240)?;
        ui.checkpoint("06-ci-progress")?;
        let merge = await_stage(ui, "CI 已通过，待人工确认合并", session, 2800)?;
        let pr = merge["pr"].as_u64().context("missing PR number")?;
        let ci = read(&evidence.join("github-ci.json"))?;
        let base = merge["branch"]
            .as_str()
            .context("missing isolated branch")?;
        ensure!(
            merge["candidate"] == candidate
                && ci["headRefOid"] == candidate
                && ci["baseRefName"] == base
                && base == format!("test/{task}")
                && ci["state"] == "OPEN"
                && ci["mergeStateStatus"] == "CLEAN",
            "CI and the merge confirmation did not identify the isolated candidate"
        );
        println!(
            "Review PR #{pr} and {}",
            evidence.join("github-ci.json").display()
        );
        ui.checkpoint("07-ci-passed")?;
        confirm(
            ui,
            &format!("merge {pr} {}", &candidate[..12]),
            "08-merge-confirmed",
        )?;

        await_stage(ui, "已合并并归档", session, 240)?;
        ui.reveal("https-pilot-page", "https-archive")?;
        ensure!(
            ui.element("https-archive")?.visible(),
            "Studio did not expose the archive"
        );
        ui.checkpoint("09-archive")?;
        let result = read(&evidence.join("result.json"))?;
        let cleanup = read(&evidence.join("cleanup.json"))?;
        let merged = read(&evidence.join("github-merged.json"))?;
        ensure!(
            result["exit_code"] == 0
                && result["candidate"] == candidate
                && cleanup["candidate_branch_removed"] == true
                && cleanup["target_synced"] == true
                && cleanup["completed_worktree_removed"] == true
                && merged["state"] == "MERGED"
                && merged["headRefOid"] == candidate
                && cleanup["merge_sha"] == merged["mergeCommit"]["oid"]
                && cleanup["merge_sha"]
                    == git(&dir.join("operator"), &["rev-parse", "HEAD"])?.trim()
                && dir.join("evidence.tar.gz").is_file(),
            "the merge or cleanup receipt did not match the confirmed candidate"
        );
        fs::copy(
            dir.join("evidence.tar.gz"),
            output.join("pilot-evidence.tar.gz"),
        )?;
        fs::copy(session.join("studio.log"), output.join("studio.log"))?;
        fs::copy(evidence.join("result.json"), output.join("result.json"))?;
        println!("Verified pilot PR #{pr}. Evidence: {}", output.display());
        Ok(())
    })
}

pub(super) fn choose(ui: &mut Driver<'_>, list: &str, id: &str) -> Result<()> {
    if !ui.element(list)?.visible() {
        ui.reveal("https-pilot-page", list)?;
    }
    let anchor = ui.element(list)?.bounds().center().y;
    let option = ui.element(id)?.bounds().center().y;
    ui.scroll(list, f32::from(anchor - option))?;
    ui.click(id)
}

pub(super) fn docker(args: &[&str]) -> Result<String> {
    let result = std::process::Command::new("docker").args(args).output()?;
    ensure!(
        result.status.success(),
        "docker {}: {}",
        args[0],
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(String::from_utf8(result.stdout)?.trim().to_owned())
}

struct Outage(String);

impl Drop for Outage {
    fn drop(&mut self) {
        if !self.0.is_empty() {
            let _ = docker(&["start", &self.0]);
        }
    }
}

#[derive(serde::Deserialize)]
struct AuthoredTask {
    title: String,
    spec: String,
    allowed_paths: Vec<String>,
    acceptance: Value,
}

fn reopen_remote(
    ui: &mut Driver<'_>,
    ws: &Workspace,
    runner: &aep_studio::Runner,
    session: &Path,
    exit: i64,
    checkpoint: &str,
) -> Result<()> {
    let previous = read(&session.join("state.json"))?;
    ui.frame(|window, _| window.remove_window())?;
    *ui = Driver::open_with_runner(ws, ui.args, runner.clone())?;
    let deadline = Instant::now() + Duration::from_secs(55);
    loop {
        let state = read(&session.join("state.json"))?;
        if state["exit_code"] == exit {
            for field in [
                "task",
                "spec",
                "baseline",
                "branch",
                "ticket",
                "candidate",
                "recovered_run",
                "pr",
                "stage",
            ] {
                ensure!(
                    state[field] == previous[field],
                    "closing Studio changed {field}"
                );
            }
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "Studio did not checkpoint the closed window"
        );
        thread::sleep(Duration::from_millis(200));
    }
    ui.click("HTTPS 试点")?;
    let name = session
        .file_name()
        .context("missing Studio session name")?
        .to_string_lossy();
    let selector = format!("https-session-{name}");
    if ui.element(&selector)?.visible() {
        ui.click(&selector)?;
    } else {
        choose(ui, "https-session-list", &selector)?;
    }
    ensure!(
        ui.absent("https-confirm")? && ui.absent("https-ack")?,
        "an approval survived closing Studio"
    );
    while ui.absent("https-resume")? {
        ensure!(Instant::now() < deadline, "the saved task is still locked");
        ui.cx.advance_clock(Duration::from_secs(1));
        ui.settle()?;
        thread::sleep(Duration::from_millis(150));
    }
    ui.reveal("https-pilot-page", "https-resume")?;
    ui.checkpoint(checkpoint)?;
    ui.click("https-resume")?;
    loop {
        let state = read(&session.join("state.json"))?;
        if state["exit_code"].is_null() {
            ensure!(
                state["ticket"] == previous["ticket"],
                "resume created a different ticket"
            );
            ui.cx.advance_clock(Duration::from_secs(1));
            ui.settle()?;
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "Studio did not restart the saved step"
        );
        thread::sleep(Duration::from_millis(150));
    }
}

pub(super) fn run_remote(mut args: Args) -> Result<()> {
    ensure!(
        io::stdin().is_terminal(),
        "--live-https-remote requires an operator terminal"
    );
    let setting = |name| std::env::var(name).with_context(|| format!("set {name}"));
    let task = setting("AEP_STUDIO_LIVE_TASK")?;
    let target = setting("AEP_STUDIO_LIVE_TARGET")?;
    let site = setting("AEP_STUDIO_LIVE_OUTAGE_SITE")?;
    let restart = std::env::var("AEP_STUDIO_LIVE_RESTART").is_ok_and(|value| value == "1");
    let retry = std::env::var("AEP_STUDIO_LIVE_RETRY").is_ok_and(|value| value == "1");
    ensure!(
        !retry || restart,
        "the failed Run rehearsal requires AEP_STUDIO_LIVE_RESTART=1"
    );
    ensure!(
        target.starts_with("test/") && aep::workspace::ident(&site, 40).is_ok(),
        "real rehearsal needs an isolated GitHub target and a named Docker site"
    );
    let ingress = format!("{site}-https");
    let control = format!("{site}-control");
    ensure!(
        docker(&[
            "inspect",
            "-f",
            "{{index .Config.Labels \"org.aep.pilot\"}}",
            &ingress
        ])? == site
            && docker(&[
                "inspect",
                "-f",
                "{{index .Config.Labels \"org.aep.pilot\"}}",
                &control
            ])? == site,
        "the outage target does not belong to the selected site"
    );
    let bind = docker(&["port", &control, "8443/tcp"])?;
    ensure!(
        bind.starts_with("127.0.0.1:")
            && setting("AEP_STUDIO_CONTROL_URL")? == format!("https://{bind}"),
        "the Studio endpoint is not the selected site's loopback HTTPS binding"
    );
    let control_argv: Vec<String> = serde_json::from_str(&docker(&[
        "inspect",
        "-f",
        "{{json .Config.Cmd}}",
        &control,
    ])?)?;
    let lease: u64 = control_argv
        .windows(2)
        .find(|pair| pair[0] == "--lease-seconds")
        .context("the deployed control plane has no explicit lease interval")?[1]
        .parse()?;
    ensure!(
        (10..=120).contains(&lease),
        "the rehearsal needs a Lease of 10–120 seconds"
    );
    let ingress_id = docker(&["inspect", "-f", "{{.Id}}", &ingress])?;
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize()?;
    let ws = Workspace::open(&source)?;
    let auth_draft: Option<AuthoredTask> = std::env::var_os("AEP_STUDIO_LIVE_DRAFT")
        .map(|path| {
            let bytes = fs::read(path)?;
            Ok::<_, anyhow::Error>(serde_json::from_slice(&bytes)?)
        })
        .transpose()?;
    let dispatch_ref = format!("refs/heads/aep-task/{task}");
    let parent = ws.data.join("https-studio");
    let existing = std::env::var("AEP_STUDIO_LIVE_FAILED_SESSION")
        .ok()
        .map(|name| -> Result<PathBuf> {
            ensure!(
                retry
                    && name.starts_with("studio-")
                    && aep::workspace::ident(&name.to_ascii_lowercase(), 63).is_ok(),
                "supply a saved failed Studio session name with retry enabled"
            );
            let session = parent.join(name);
            let state = read(&session.join("state.json"))?;
            ensure!(
                state["remote"] == true
                    && state["task"] == task
                    && state["branch"] == target
                    && state["exit_code"] == 1
                    && state["pr"].is_null(),
                "the saved session is not the scoped failed rehearsal"
            );
            Ok(session)
        })
        .transpose()?;
    if auth_draft.is_some() && existing.is_none() {
        ensure!(
            git(&ws.root, &["ls-remote", "--refs", "origin", &dispatch_ref])?
                .trim()
                .is_empty(),
            "the authored rehearsal needs a fresh task ID"
        );
    }
    let before = sessions(&parent);
    let output = args
        .output_dir
        .as_ref()
        .context("missing checkpoint directory")?;
    fs::create_dir_all(output)?;
    let output = tempfile::Builder::new()
        .prefix("studio-remote-")
        .tempdir_in(output.canonicalize()?)?
        .keep();
    args.output_dir = Some(output.clone());
    args.screenshots = true;
    let runner = aep_studio::Runner {
        binary: Some(crate::handoff_fixture::build_runner()?),
        client_bin: None,
    };
    let mut ui = Driver::open_with_runner(&ws, &args, runner.clone())?;
    println!("Studio checkpoints: {}", output.display());
    let result = (|| {
        ui.click("HTTPS 试点")?;
        let session = if let Some(session) = &existing {
            session.clone()
        } else {
            ui.click("https-real-mode")?;
            let selected = format!("https-task-{task}");
            let target_id = format!("https-target-{target}");
            let choice = if auth_draft.is_some() {
                &target_id
            } else {
                &selected
            };
            let deadline = Instant::now() + Duration::from_secs(45);
            while ui.absent(choice)? {
                ensure!(
                    Instant::now() < deadline,
                    "Studio did not load the selected task or target"
                );
                ui.settle()?;
                thread::sleep(Duration::from_millis(150));
            }
            if auth_draft.is_none() {
                choose(&mut ui, "https-task-list", &selected)?;
            }
            choose(&mut ui, "https-target-list", &target_id)?;
            if let Some(draft) = &auth_draft {
                ui.reveal("https-pilot-page", "https-open-authoring")?;
                ui.click("https-open-authoring")?;
                let paths = draft.allowed_paths.join("\n");
                let acceptance = serde_json::to_string(&draft.acceptance)?;
                for (id, value) in [
                    ("task-author-name", task.as_str()),
                    ("task-author-title", draft.title.as_str()),
                    ("task-author-spec", draft.spec.as_str()),
                    ("task-author-paths", paths.as_str()),
                    ("task-author-check", acceptance.as_str()),
                ] {
                    ui.reveal("https-pilot-page", id)?;
                    ui.click(id)?;
                    ui.input(value)?;
                }
                ui.reveal("https-pilot-page", "https-create-and-dispatch")?;
                ui.checkpoint("01-task-authored")?;
                ui.click("https-create-and-dispatch")?;
            } else {
                ui.reveal("https-pilot-page", "https-dispatch")?;
                ui.checkpoint("01-task-target-selected")?;
                ui.click("https-dispatch")?;
            }
            ui.scroll("https-pilot-page", 2_000.)?;

            let created = sessions(&parent);
            let new: Vec<_> = created.difference(&before).collect();
            ensure!(new.len() == 1, "Studio did not start exactly one real task");
            new[0].clone()
        };
        let session = session.as_path();
        let dir = session.join(&task);
        let evidence = dir.join("evidence");
        println!("Remote task session: {}", dir.display());

        let saved_failure = read(&session.join("state.json")).unwrap_or_default();
        let (pending, old) = if existing.is_some() && saved_failure["stage"] == "publication" {
            reopen_remote(&mut ui, &ws, &runner, session, 1, "05b-publication-retry")?;
            let restored = await_stage(&mut ui, "待人工确认发布", session, 120)?;
            for field in ["ticket", "spec", "candidate", "recovered_run"] {
                ensure!(
                    restored[field] == saved_failure[field],
                    "publication retry changed {field}"
                );
            }
            let prior_retry = read(&evidence.join("retry.json"))?;
            let previous = prior_retry["data"]["predecessor"]
                .as_str()
                .context("missing interrupted Run")?;
            (restored, previous.to_owned())
        } else {
            let failed_run = if retry {
                let deadline = Instant::now() + Duration::from_secs(850);
                let failed = loop {
                    ui.cx.advance_clock(Duration::from_secs(1));
                    ui.settle()?;
                    let state = read(&session.join("state.json")).unwrap_or_default();
                    if state["exit_code"] == 1 && !ui.absent("https-failure")? {
                        break state;
                    }
                    ensure!(
                        Instant::now() < deadline,
                        "the first Run did not show the injected acceptance failure"
                    );
                    thread::sleep(Duration::from_millis(200));
                };
                let id = failed["recovered_run"]
                    .as_str()
                    .context("missing failed Run")?
                    .to_owned();
                let checks = evidence.join("worker/runs").join(&id).join("checks.log");
                ensure!(
                    fs::read_to_string(&checks)?
                        .contains("Studio retry rehearsal: first acceptance blocked"),
                    "the first Run failed for a different reason"
                );
                ensure!(
                    ui.element("https-resume")?.label() == Some("重试当前步骤"),
                    "failed task has no retry action"
                );
                ui.reveal("https-pilot-page", "https-failure")?;
                ui.checkpoint("01b-failed-run")?;
                docker(&[
                    "exec",
                    &format!("{site}-worker"),
                    "/bin/sh",
                    "-ec",
                    r#"if [ -e "$1" ]; then exec unlink "$1"; fi"#,
                    "unlink-rehearsal-flag",
                    &format!("/pilot/tmp/aep-studio-retry-{task}"),
                ])?;
                reopen_remote(&mut ui, &ws, &runner, session, 1, "01c-reopen-and-retry")?;
                Some(id)
            } else {
                None
            };
            let checking = await_stage(&mut ui, "远程 Worker 正在验收固定候选", session, 850)?;
            let candidate = checking["candidate"]
                .as_str()
                .filter(|sha| sha.len() == 40)
                .context("missing remote candidate")?;
            ensure!(
                checking["task"] == task && checking["branch"] == target,
                "the remote Run selected a different scope"
            );
            if let Some(draft) = &auth_draft {
                let authoring = read(&evidence.join("authoring.json"))?;
                let spec = checking["spec"]
                    .as_str()
                    .context("missing created Spec SHA")?;
                let plan = read(&evidence.join("plan.json"))?;
                let manifest = read(&evidence.join("task.json"))?;
                ensure!(
                    authoring["task"] == task
                        && authoring["spec"] == spec
                        && authoring["target"] == target
                        && authoring["baseline"] == plan["baseline"]
                        && plan["spec"] == spec
                        && manifest["title"] == draft.title
                        && manifest["allowed_paths"] == serde_json::json!(draft.allowed_paths)
                        && manifest["check"] == draft.acceptance["check"]
                        && manifest["criteria"] == draft.acceptance["criteria"]
                        && fs::read_to_string(evidence.join("spec.md"))?.trim_end()
                            == draft.spec.trim_end()
                        && git(&ws.root, &["ls-remote", "--refs", "origin", &dispatch_ref])?.trim()
                            == format!("{spec}\t{dispatch_ref}"),
                    "Studio did not create and dispatch the exact authored Spec"
                );
                fs::copy(
                    evidence.join("authoring.json"),
                    output.join("authored-task.json"),
                )?;
            }
            ui.checkpoint("02-remote-worker")?;
            let current = read(&evidence.join("control-status.json"))?;
            let run = checking["recovered_run"]
                .as_str()
                .context("missing scoped Run")?;
            if let Some(failed) = &failed_run {
                let retry = read(&evidence.join("retry.json"))?;
                ensure!(
                    retry["data"]["predecessor"] == failed.as_str()
                        && retry["data"]["id"] == run
                        && retry["data"]["reverify"] == false,
                    "retry did not continue the failed task"
                );
            }
            let attempts = current["data"]["attempts"]
                .as_array()
                .context("missing current ticket status")?;
            ensure!(
                attempts
                    .iter()
                    .any(|attempt| attempt["id"] == run && attempt["stage"] == "running")
                    && attempts
                        .iter()
                        .filter(|attempt| attempt["id"] != run)
                        .all(|attempt| {
                            matches!(
                                attempt["stage"].as_str(),
                                Some(
                                    "interrupted"
                                        | "cancelled"
                                        | "failed"
                                        | "blocked"
                                        | "published"
                                        | "superseded"
                                )
                            )
                        }),
                "another ticket is active, or the scoped Run is no longer checking"
            );
            docker(&["stop", "-t", "2", &ingress_id])?;
            let mut outage = Outage(ingress_id.clone());
            let disconnected_at = Instant::now();
            await_stage(&mut ui, "HTTPS 连接中断，等待控制面恢复", session, 18)?;
            ui.checkpoint("03-https-disconnected")?;
            thread::sleep(Duration::from_secs(lease + 4).saturating_sub(disconnected_at.elapsed()));
            docker(&["start", &ingress_id])?;
            outage.0.clear();
            await_stage(
                &mut ui,
                "对同一候选重新验收，运行 Claude 审查",
                session,
                180,
            )?;
            ui.checkpoint("04-recovery")?;

            let pending = await_stage(&mut ui, "待人工确认发布", session, 850)?;
            if restart {
                reopen_remote(
                    &mut ui,
                    &ws,
                    &runner,
                    session,
                    4,
                    "04b-reopened-before-publication",
                )?;
                let restored = await_stage(&mut ui, "待人工确认发布", session, 90)?;
                for field in ["ticket", "spec", "candidate", "recovered_run"] {
                    ensure!(
                        restored[field] == pending[field],
                        "publication restored a different {field}"
                    );
                }
            }
            ensure!(
                pending["candidate"] == candidate,
                "the recovered Run changed the candidate"
            );
            (pending, run.to_owned())
        };
        let candidate = pending["candidate"]
            .as_str()
            .context("missing approved candidate")?;
        let next = pending["recovered_run"]
            .as_str()
            .context("missing recovery Run")?;
        let first = read(
            &evidence
                .join("worker/runs")
                .join(&old)
                .join("evidence.json"),
        )?;
        let recovered = read(
            &evidence
                .join("worker/runs")
                .join(next)
                .join("evidence.json"),
        )?;
        ensure!(
            old != next
                && first["candidate_sha"] == candidate
                && recovered["candidate_sha"] == candidate
                && first["usage"]["implementer"]["output_tokens"]
                    .as_u64()
                    .is_some_and(|n| n > 0)
                && recovered["usage"]["reviewer"]["output_tokens"]
                    .as_u64()
                    .is_some_and(|n| n > 0)
                && recovered["check_exit_code"] == 0
                && read(&evidence.join("stale-denied.json"))?["error"]
                    .as_str()
                    .is_some_and(|error| error.contains("stale Run")),
            "remote Worker evidence does not prove the two attempts"
        );
        let retry = read(&evidence.join("retry.json"))?;
        ensure!(
            retry["data"]["predecessor"] == old && retry["data"]["reverify"] == true,
            "remote recovery did not continue from the interrupted Run"
        );
        ensure!(
            !evidence.join("publication.json").exists(),
            "published before confirmation"
        );
        println!(
            "Review {} and recovered checks.log and review.txt",
            evidence.join("candidate.diff").display()
        );
        ui.checkpoint("05-publication-review")?;
        confirm(
            &mut ui,
            &format!("publish {}", &candidate[..12]),
            "06-publication-approved",
        )?;
        await_stage(&mut ui, "等待并核对 GitHub CI", session, 240)?;
        ui.checkpoint("07-ci-progress")?;

        let pending = await_stage(&mut ui, "CI 已通过，待人工确认合并", session, 2800)?;
        if restart {
            reopen_remote(
                &mut ui,
                &ws,
                &runner,
                session,
                4,
                "07b-reopened-before-merge",
            )?;
            let restored = await_stage(&mut ui, "CI 已通过，待人工确认合并", session, 120)?;
            for field in ["ticket", "candidate", "pr"] {
                ensure!(
                    restored[field] == pending[field],
                    "merge restored a different {field}"
                );
            }
        }
        let number = pending["pr"].as_u64().context("missing GitHub PR")?;
        let ci = read(&evidence.join("github-ci.json"))?;
        ensure!(
            ci["headRefOid"] == candidate
                && ci["baseRefName"] == target
                && ci["mergeStateStatus"] == "CLEAN",
            "GitHub CI did not match the selected target and candidate"
        );
        println!(
            "Review PR #{number} and {}",
            evidence.join("github-ci.json").display()
        );
        ui.checkpoint("08-ci-passed")?;
        confirm(
            &mut ui,
            &format!("merge {number} {}", &candidate[..12]),
            "09-merge-approved",
        )?;
        await_stage(&mut ui, "已合并并归档", session, 240)?;
        ui.reveal("https-pilot-page", "https-archive")?;
        ensure!(
            ui.element("https-archive")?.visible(),
            "Studio did not show the archive"
        );
        ui.checkpoint("10-archive")?;
        let cleanup = read(&evidence.join("cleanup.json"))?;
        let merge = read(&evidence.join("github-merged.json"))?;
        ensure!(
            merge["state"] == "MERGED"
                && merge["headRefOid"] == candidate
                && cleanup["merge_sha"] == merge["mergeCommit"]["oid"]
                && cleanup["target_synced"] == true
                && cleanup["candidate_branch_removed"] == true,
            "the remote task merge and cleanup did not match the approval"
        );
        fs::copy(
            dir.join("evidence.tar.gz"),
            output.join("pilot-evidence.tar.gz"),
        )?;
        fs::copy(evidence.join("result.json"), output.join("result.json"))?;
        println!(
            "Verified remote task PR #{number}. Evidence: {}",
            output.display()
        );
        Ok(())
    })();
    if result.is_err() {
        let _ = ui.checkpoint("failure");
    }
    result
}
