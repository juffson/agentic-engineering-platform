use super::{Args, Driver, handoff_fixture};
use aep::{
    deploy_keys::WorkerKeys,
    workspace::{Workspace, ident},
};
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    io::{self, IsTerminal, Write},
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant},
};

fn pause(ui: &mut Driver<'_>) -> Result<()> {
    ui.cx.advance_clock(Duration::from_millis(500));
    ui.settle()?;
    thread::sleep(Duration::from_millis(250));
    Ok(())
}

fn click(ui: &mut Driver<'_>, id: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(45);
    while ui.absent(id)? {
        ensure!(Instant::now() < deadline, "Studio did not show {id}");
        pause(ui)?;
    }
    ui.reveal("https-pilot-page", id)?;
    ui.click(id)
}

fn complete(
    ui: &mut Driver<'_>,
    status: &impl Fn() -> Result<WorkerKeys>,
    before: &WorkerKeys,
) -> Result<WorkerKeys> {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        pause(ui)?;
        let after = status()?;
        if after.last != before.last && after.request.is_none() {
            let receipt = after
                .last
                .as_ref()
                .context("missing Worker operation receipt")?;
            ensure!(
                receipt.error.is_none(),
                "{}",
                receipt.error.as_deref().unwrap_or_default()
            );
            return Ok(after);
        }
        ensure!(
            Instant::now() < deadline,
            "Studio Deploy Key action did not finish"
        );
    }
}

fn checkpoint(
    ui: &mut Driver<'_>,
    output: &Path,
    step: &str,
    worker: &str,
    keys: &WorkerKeys,
) -> Result<()> {
    ui.reveal("https-pilot-page", &format!("https-key-scope-{worker}"))?;
    ui.checkpoint(step)?;
    fs::write(
        output.join(format!("{step}-keys.json")),
        serde_json::to_vec_pretty(keys)?,
    )?;
    Ok(())
}

pub(super) fn run(mut args: Args) -> Result<()> {
    let worker = args.https_key_worker.clone().context("missing Worker")?;
    ident(&worker, 63)?;
    let ws = Workspace::open(
        &args
            .https_remote_repo
            .as_ref()
            .context("missing repository")?
            .canonicalize()?,
    )?;
    let dir = args
        .output_dir
        .as_ref()
        .context("missing proof directory")?;
    fs::create_dir_all(dir)?;
    let output = tempfile::Builder::new()
        .prefix("studio-keys-")
        .tempdir_in(dir)?
        .keep();
    args.output_dir = Some(output.clone());
    args.screenshots = true;
    let executable = handoff_fixture::build_runner()?;
    let endpoint = std::env::var("AEP_STUDIO_CONTROL_URL")?;
    ensure!(endpoint.starts_with("https://"), "use the live HTTPS site");
    let operator = fs::read_to_string(
        std::env::var_os("AEP_STUDIO_CONTROL_OPERATOR_FILE")
            .context("missing operator token file")?,
    )?;
    let ca = std::env::var_os("AEP_STUDIO_CONTROL_CA");
    let status = || -> Result<WorkerKeys> {
        let mut command = Command::new(&executable);
        command
            .arg("--repo")
            .arg(&ws.root)
            .args(["control", "--url", &endpoint])
            .env("AEP_CONTROL_OPERATOR_TOKEN", operator.trim())
            .env_remove("AEP_CONTROL_DEVICE_TOKEN");
        if let Some(ca) = &ca {
            command.arg("--ca-cert").arg(ca);
        }
        let result = command.arg("status").output()?;
        ensure!(result.status.success(), "cannot read the live HTTPS site");
        let value: serde_json::Value = serde_json::from_slice(&result.stdout)?;
        let keys = value
            .pointer(&format!("/data/deploy_keys/{worker}"))
            .context("selected Worker does not manage Deploy Keys")?;
        Ok(serde_json::from_value(keys.clone())?)
    };
    let runner = aep_studio::Runner {
        binary: Some(executable.clone()),
        client_bin: None,
    };
    let mut ui = Driver::open_with_runner(&ws, &args, runner)?;
    ui.click("HTTPS 试点")?;
    ui.click("https-real-mode")?;
    click(&mut ui, "https-connect-queue")?;
    let scope = format!("https-key-scope-{worker}");
    let deadline = Instant::now() + Duration::from_secs(45);
    while ui.absent(&scope)? {
        ensure!(
            Instant::now() < deadline,
            "Studio did not display the selected Worker"
        );
        pause(&mut ui)?;
    }
    let mut state = status()?;
    checkpoint(&mut ui, &output, "01-worker-keys", &worker, &state)?;
    if args.https_key_revoke {
        ensure!(
            io::stdin().is_terminal(),
            "retirement requires an operator terminal"
        );
        let old = state
            .inventory
            .retiring
            .as_ref()
            .context("no old Deploy Key is awaiting retirement")?;
        let current = state
            .inventory
            .active
            .as_ref()
            .context("missing active key")?;
        ensure!(
            state.verified_at.is_some(),
            "the new Deploy Key has yet to be verified"
        );
        click(&mut ui, &format!("https-key-review-revoke-{worker}"))?;
        ui.checkpoint("02-review-retirement")?;
        let confirmation = format!("revoke {}", old.fingerprint);
        print!(
            "Active {}. Type {confirmation} to confirm retirement: ",
            current.fingerprint
        );
        io::stdout().flush()?;
        let mut response = String::new();
        io::stdin().read_line(&mut response)?;
        ensure!(
            response.trim() == confirmation,
            "operator declined retirement"
        );
        click(&mut ui, &format!("https-key-confirm-revoke-{worker}"))?;
        let after = complete(&mut ui, &status, &state)?;
        ensure!(
            after.inventory.retiring.is_none() && after.inventory.active == state.inventory.active,
            "retirement changed the active Deploy Key"
        );
        click(&mut ui, &format!("https-key-verify-{worker}"))?;
        state = complete(&mut ui, &status, &after)?;
        ensure!(
            state.verified_at.is_some(),
            "new Deploy Key failed verification after retirement"
        );
        checkpoint(&mut ui, &output, "03-retired-and-verified", &worker, &state)?;
    } else {
        // A fresh Worker starts with no key. Configure its first key before rotating.
        let count = if state.inventory.active.is_none() {
            2
        } else {
            1
        };
        for iteration in 1..=count {
            let previous = state.inventory.active.clone();
            if state.inventory.staged.is_none() {
                click(&mut ui, &format!("https-key-prepare-{worker}"))?;
                state = complete(&mut ui, &status, &state)?;
            }
            let staged = state
                .inventory
                .staged
                .clone()
                .context("missing staged Deploy Key")?;
            checkpoint(
                &mut ui,
                &output,
                &format!("02-{iteration}-key-staged"),
                &worker,
                &state,
            )?;
            click(&mut ui, &format!("https-key-install-{worker}"))?;
            state = complete(&mut ui, &status, &state)?;
            ensure!(
                state.inventory.active == Some(staged)
                    && state.inventory.retiring == previous
                    && state.verified_at.is_some(),
                "Worker failed to switch to the verified Deploy Key"
            );
            click(&mut ui, &format!("https-key-verify-{worker}"))?;
            state = complete(&mut ui, &status, &state)?;
            ensure!(
                state.verified_at.is_some(),
                "GitHub read-only verification did not complete"
            );
            checkpoint(
                &mut ui,
                &output,
                &format!("03-{iteration}-active-verified"),
                &worker,
                &state,
            )?;
        }
    }
    println!(
        "Verified Studio Deploy Key operations for {worker}. Evidence: {}",
        output.display()
    );
    Ok(())
}
