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

fn sessions(parent: &Path) -> HashSet<PathBuf> {
    fs::read_dir(parent)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|ty| ty.is_dir()))
        .map(|entry| entry.path())
        .collect()
}

fn await_stage(ui: &mut Driver<'_>, label: &str, session: &Path, seconds: u64) -> Result<Value> {
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

fn confirm(ui: &mut Driver<'_>, action: &str, checkpoint: &str) -> Result<()> {
    ensure!(
        ui.absent("https-confirm")?,
        "Studio exposed an unconfirmed action"
    );
    print!("Type {action} to approve this Studio action: ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    ensure!(answer.trim() == action, "operator declined {action}");
    ui.click("https-ack")?;
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

fn choose(ui: &mut Driver<'_>, list: &str, id: &str) -> Result<()> {
    if !ui.element(list)?.visible() {
        ui.reveal("https-pilot-page", list)?;
    }
    let anchor = ui.element(list)?.bounds().center().y;
    let option = ui.element(id)?.bounds().center().y;
    ui.scroll(list, f32::from(anchor - option))?;
    ui.click(id)
}

fn docker(args: &[&str]) -> Result<String> {
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

pub(super) fn run_remote(mut args: Args) -> Result<()> {
    ensure!(
        io::stdin().is_terminal(),
        "--live-https-remote requires an operator terminal"
    );
    let setting = |name| std::env::var(name).with_context(|| format!("set {name}"));
    let task = setting("AEP_STUDIO_LIVE_TASK")?;
    let target = setting("AEP_STUDIO_LIVE_TARGET")?;
    let site = setting("AEP_STUDIO_LIVE_OUTAGE_SITE")?;
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
    let parent = ws.data.join("https-studio");
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
    let mut ui = Driver::open_with_runner(&ws, &args, runner)?;
    println!("Studio checkpoints: {}", output.display());
    let result = (|| {
        ui.click("HTTPS 试点")?;
        ui.click("https-real-mode")?;
        let selected = format!("https-task-{task}");
        let deadline = Instant::now() + Duration::from_secs(45);
        while ui.absent(&selected)? {
            ensure!(
                Instant::now() < deadline,
                "Studio did not load the pinned task"
            );
            ui.settle()?;
            thread::sleep(Duration::from_millis(150));
        }
        choose(&mut ui, "https-task-list", &selected)?;
        choose(
            &mut ui,
            "https-target-list",
            &format!("https-target-{target}"),
        )?;
        ui.reveal("https-pilot-page", "https-dispatch")?;
        ui.checkpoint("01-task-target-selected")?;
        ui.click("https-dispatch")?;
        ui.scroll("https-pilot-page", 2_000.)?;

        let created = sessions(&parent);
        let new: Vec<_> = created.difference(&before).collect();
        ensure!(new.len() == 1, "Studio did not start exactly one real task");
        let session = new[0];
        let dir = session.join(&task);
        let evidence = dir.join("evidence");
        println!("Remote task session: {}", dir.display());

        let checking = await_stage(&mut ui, "远程 Worker 正在验收固定候选", session, 850)?;
        let candidate = checking["candidate"]
            .as_str()
            .filter(|sha| sha.len() == 40)
            .context("missing remote candidate")?;
        ensure!(
            checking["task"] == task && checking["branch"] == target,
            "the remote Run selected a different scope"
        );
        ui.checkpoint("02-remote-worker")?;
        let current = read(&evidence.join("control-status.json"))?;
        let run = checking["old_run"].as_str().context("missing scoped Run")?;
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
        ensure!(
            pending["candidate"] == candidate,
            "the recovered Run changed the candidate"
        );
        let old = pending["old_run"]
            .as_str()
            .context("missing interrupted Run")?;
        let next = pending["recovered_run"]
            .as_str()
            .context("missing recovery Run")?;
        let first = read(&evidence.join("worker/runs").join(old).join("evidence.json"))?;
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
