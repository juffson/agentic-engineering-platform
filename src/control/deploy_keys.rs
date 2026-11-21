use super::{
    http::Client,
    state::{Operation, Reply},
};
use aep::{
    delivery,
    deploy_keys::{Change, Key},
    workspace::{Workspace, ident},
};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::ffi::OsStr;

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Action {
    Prepare,
    Install,
    Verify,
    Revoke,
}

#[derive(clap::Args)]
pub struct Command {
    #[arg(long)]
    worker: String,
    #[arg(long)]
    installation: String,
    #[arg(long, value_enum)]
    action: Action,
    #[arg(long)]
    fingerprint: Option<String>,
    #[arg(long)]
    rotation: Option<String>,
}

#[derive(Deserialize)]
struct GitHubKey {
    id: u64,
    key: String,
    read_only: bool,
    #[serde(default = "enabled")]
    enabled: bool,
}

fn enabled() -> bool {
    true
}

fn lookup<'a>(keys: &'a [GitHubKey], key: &Key) -> Option<&'a GitHubKey> {
    let mut parts = key.public.split_whitespace();
    let algorithm = parts.next();
    let data = parts.next();
    keys.iter().find(|key| {
        let mut parts = key.key.split_whitespace();
        parts.next() == algorithm && parts.next() == data
    })
}

fn require_read_only<'a>(keys: &'a [GitHubKey], key: &Key) -> Result<&'a GitHubKey> {
    let remote = lookup(keys, key)
        .context("this Deploy Key is not registered in the selected GitHub repository")?;
    ensure!(
        remote.read_only && remote.enabled,
        "GitHub did not confirm an enabled read-only Deploy Key"
    );
    Ok(remote)
}

pub fn execute(client: &Client, ws: &Workspace, path: &OsStr, command: Command) -> Result<Reply> {
    let Command {
        worker,
        installation,
        action,
        fingerprint,
        rotation,
    } = command;
    ident(&worker, 63)?;
    ident(&installation, 64)?;
    let repository = delivery::repository(ws, path)?;
    let Reply::Snapshot(state) = client.call(Operation::Status)? else {
        anyhow::bail!("invalid control status");
    };
    ensure!(
        state.workers.get(&worker).is_some_and(|device| {
            device.online == Some(true) && device.installation == installation
        }),
        "the selected Worker is offline or its installation changed"
    );
    let expected = fingerprint.unwrap_or_default();
    let change = match action {
        Action::Prepare => Change::Prepare,
        Action::Install => Change::Activate {
            fingerprint: expected.clone(),
        },
        Action::Verify => Change::Verify {
            fingerprint: expected.clone(),
        },
        Action::Revoke => Change::Forget {
            fingerprint: expected.clone(),
        },
    };
    let entry = state.authorize_key_change(
        &repository,
        &worker,
        &installation,
        rotation.as_deref(),
        &change,
    )?;
    let keys_url = format!("repos/{repository}/keys");
    let list = || -> Result<Vec<GitHubKey>> {
        let value = delivery::github(ws, path, &[&keys_url, "--paginate", "--slurp"])?;
        let pages: Vec<Vec<GitHubKey>> = serde_json::from_value(value)?;
        Ok(pages.into_iter().flatten().collect())
    };
    match action {
        Action::Prepare => (),
        Action::Install => {
            let staged = entry
                .inventory
                .staged
                .as_ref()
                .context("missing staged Deploy Key")?;
            let before = list()?;
            if lookup(&before, staged).is_none() {
                let created = delivery::github(
                    ws,
                    path,
                    &[
                        &keys_url,
                        "--method",
                        "POST",
                        "-f",
                        &format!("title=AEP {worker}"),
                        "-f",
                        &format!("key={}", staged.public),
                        "-F",
                        "read_only=true",
                    ],
                );
                let after = list()?;
                if lookup(&after, staged).is_none() {
                    created?;
                }
                require_read_only(&after, staged)?;
            } else {
                require_read_only(&before, staged)?;
            }
        }
        Action::Verify => {
            let active = entry
                .inventory
                .active
                .as_ref()
                .context("missing active Deploy Key")?;
            require_read_only(&list()?, active)?;
        }
        Action::Revoke => {
            let active = entry
                .inventory
                .active
                .as_ref()
                .context("missing active Deploy Key")?;
            let retiring = entry
                .inventory
                .retiring
                .as_ref()
                .context("missing retiring Deploy Key")?;
            let before = list()?;
            require_read_only(&before, active)?;
            if let Some(old) = lookup(&before, retiring) {
                let removed = delivery::command(
                    ws,
                    path,
                    "gh",
                    &[
                        "api",
                        "--hostname",
                        "github.com",
                        "--method",
                        "DELETE",
                        &format!("{keys_url}/{}", old.id),
                    ],
                );
                let after = list()?;
                if lookup(&after, retiring).is_some() {
                    removed?;
                    anyhow::bail!("GitHub still lists the retiring Deploy Key");
                }
                require_read_only(&after, active)?;
            }
        }
    }
    client.call(Operation::KeyRequest {
        repository,
        worker,
        installation,
        change,
        rotation,
    })
}
