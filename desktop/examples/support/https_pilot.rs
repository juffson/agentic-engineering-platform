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
