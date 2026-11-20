use super::{
    http::{Client, DEVICE_TOKEN, OPERATOR_TOKEN},
    state::{Operation, Profile, Reply, save},
};
use aep::{
    deploy_keys::{Change, Inventory, Key, Receipt, Request},
    workspace::ident,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Clone, Deserialize, Serialize)]
struct Slot {
    key: Key,
    // The legacy provisioned private key lives in the read-only device mount.
    file: Option<String>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
struct Local {
    active: Option<Slot>,
    staged: Option<Slot>,
    retiring: Option<Slot>,
    last: Option<Receipt>,
}

impl Local {
    fn inventory(&self) -> Inventory {
        Inventory {
            active: self.active.as_ref().map(|slot| slot.key.clone()),
            staged: self.staged.as_ref().map(|slot| slot.key.clone()),
            retiring: self.retiring.as_ref().map(|slot| slot.key.clone()),
        }
    }
}

pub struct Manager<'a> {
    directory: &'a Path,
    legacy: Option<&'a Path>,
    fetcher: &'a Path,
    checkout: &'a Path,
    repository: String,
    state: Local,
}

fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut process = Command::new(program);
    process
        .env_remove(OPERATOR_TOKEN)
        .env_remove(DEVICE_TOKEN)
        .stdin(Stdio::null());
    process
}

fn public_key(path: &Path) -> Result<Key> {
    let output = command("/usr/bin/ssh-keygen")
        .args(["-y", "-P", "", "-f"])
        .arg(path)
        .output()?;
    ensure!(output.status.success(), "cannot load Deploy Key");
    let public = String::from_utf8(output.stdout)?
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    let output = command("/usr/bin/ssh-keygen")
        .args(["-l", "-f"])
        .arg(path)
        .output()?;
    ensure!(output.status.success(), "cannot fingerprint Deploy Key");
    let fingerprint = String::from_utf8(output.stdout)?
        .split_whitespace()
        .nth(1)
        .context("missing Deploy Key fingerprint")?
        .to_owned();
    let key = Key {
        public,
        fingerprint,
    };
    key.validate()?;
    Ok(key)
}

impl<'a> Manager<'a> {
    pub fn open(
        directory: &'a Path,
        legacy: Option<&'a Path>,
        fetcher: &'a Path,
        checkout: &'a Path,
        repository: String,
    ) -> Result<Self> {
        if !directory.try_exists()? {
            fs::DirBuilder::new().mode(0o700).create(directory)?;
        }
        let metadata = fs::symlink_metadata(directory)?;
        ensure!(
            metadata.is_dir() && metadata.permissions().mode() & 0o077 == 0,
            "Deploy Key directory must be private"
        );
        let file = directory.join("keys.json");
        let state = match fs::read(&file) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let active = if let Some(path) = legacy {
                    Some(Slot {
                        key: public_key(path)?,
                        file: None,
                    })
                } else {
                    None
                };
                let state = Local {
                    active,
                    ..Local::default()
                };
                save(&file, &state)?;
                state
            }
            Err(error) => return Err(error.into()),
        };
        state.inventory().validate()?;
        Ok(Self {
            directory,
            legacy,
            fetcher,
            checkout,
            repository,
            state,
        })
    }

    fn path(&self, slot: &Slot) -> Result<PathBuf> {
        match &slot.file {
            Some(file) => {
                ident(file, 64)?;
                Ok(self.directory.join(file))
            }
            None => self
                .legacy
                .map(Path::to_path_buf)
                .context("missing legacy Deploy Key"),
        }
    }

    pub fn active(&self) -> Result<Option<PathBuf>> {
        self.state
            .active
            .as_ref()
            .map(|slot| self.path(slot))
            .transpose()
    }

    fn probe(&self, slot: &Slot) -> Result<()> {
        let output = command(self.fetcher)
            .arg(self.checkout)
            .args([&self.repository, "--probe", "HEAD"])
            .env("AEP_PILOT_GITHUB_KEY_FILE", self.path(slot)?)
            .output()?;
        ensure!(
            output.status.success() && !output.stdout.is_empty(),
            "GitHub could not be read with this Deploy Key"
        );
        Ok(())
    }

    fn change(&self, next: &mut Local, request: &Request) -> Result<()> {
        request.change.validate(&next.inventory())?;
        match &request.change {
            Change::Prepare => {
                let path = self.directory.join(&request.id);
                if !path.try_exists()? {
                    let status = command("/usr/bin/ssh-keygen")
                        .args([
                            "-q",
                            "-t",
                            "ed25519",
                            "-N",
                            "",
                            "-C",
                            "AEP Worker Deploy Key",
                            "-f",
                        ])
                        .arg(&path)
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status()?;
                    ensure!(status.success(), "could not generate a Deploy Key");
                }
                next.staged = Some(Slot {
                    key: public_key(&path)?,
                    file: Some(request.id.clone()),
                });
            }
            Change::Activate { .. } => {
                let staged = next.staged.as_ref().context("missing staged Deploy Key")?;
                self.probe(staged)?;
                next.retiring = next.active.take();
                next.active = next.staged.take();
            }
            Change::Verify { .. } => {
                self.probe(next.active.as_ref().context("missing active Deploy Key")?)?
            }
            Change::Forget { .. } => {
                let retiring = next
                    .retiring
                    .as_ref()
                    .context("missing retiring Deploy Key")?;
                if retiring.file.is_some() {
                    let path = self.path(retiring)?;
                    for file in [path.clone(), path.with_extension("pub")] {
                        match fs::remove_file(file) {
                            Ok(()) => (),
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                            Err(error) => return Err(error.into()),
                        }
                    }
                }
                next.retiring = None;
            }
        }
        Ok(())
    }

    pub fn sync(&mut self, client: &Client, installation: &str, profile: &Profile) -> Result<()> {
        let reply = client.call(Operation::KeySync {
            installation: installation.into(),
            profile: profile.clone(),
            repository: self.repository.clone(),
            inventory: self.state.inventory(),
            receipt: self.state.last.clone(),
        })?;
        let Reply::KeyWork(request) = reply else {
            anyhow::bail!("unexpected Deploy Key response");
        };
        if let Some(request) = request
            && self
                .state
                .last
                .as_ref()
                .is_none_or(|ack| ack.id != request.id)
        {
            ident(&request.id, 64)?;
            let mut next = self.state.clone();
            let error = self
                .change(&mut next, &request)
                .err()
                .map(|error| error.to_string());
            next.last = Some(Receipt {
                id: request.id,
                error,
            });
            save(&self.directory.join("keys.json"), &next)?;
            self.state = next;
        }
        Ok(())
    }
}
