//! Personal access tokens for Studio plugins.
use anyhow::{Context, Result, anyhow, bail, ensure};
use std::{
    collections::HashMap,
    io::Write,
    process::{Command, Output, Stdio},
    sync::Mutex,
};

/// Stores tokens by Keychain account. Callers choose the account scope.
pub trait Secrets: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<String>>;
    fn set(&self, account: &str, token: &str) -> Result<()>;
    fn delete(&self, account: &str) -> Result<()>;
}

pub fn valid_token(token: &str) -> bool {
    (8..=512).contains(&token.len())
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

#[derive(Default)]
pub struct Memory(Mutex<HashMap<String, String>>);

impl Secrets for Memory {
    fn get(&self, account: &str) -> Result<Option<String>> {
        let tokens = self.0.lock().map_err(|_| anyhow!("lock plugin tokens"))?;
        Ok(tokens.get(account).cloned())
    }

    fn set(&self, account: &str, token: &str) -> Result<()> {
        ensure!(valid_token(token), "invalid plugin token");
        let mut tokens = self.0.lock().map_err(|_| anyhow!("lock plugin tokens"))?;
        tokens.insert(account.to_owned(), token.to_owned());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<()> {
        let mut tokens = self.0.lock().map_err(|_| anyhow!("lock plugin tokens"))?;
        tokens.remove(account);
        Ok(())
    }
}

pub struct Keychain {
    pub service: String,
}

impl Default for Keychain {
    fn default() -> Self {
        Self {
            service: "AEP Studio plugin token".to_owned(),
        }
    }
}

impl Secrets for Keychain {
    fn get(&self, account: &str) -> Result<Option<String>> {
        let output = self.run(Operation::Get, account)?;
        if output.status.code() == Some(44) {
            return Ok(None);
        }
        check_status(&output, "read")?;
        let token = String::from_utf8(output.stdout)
            .map_err(|_| anyhow!("invalid token data from macOS Keychain"))?;
        Ok(Some(token.trim_end_matches(['\r', '\n']).to_owned()))
    }

    fn set(&self, account: &str, token: &str) -> Result<()> {
        check_status(&self.run(Operation::Set(token), account)?, "write")
    }

    fn delete(&self, account: &str) -> Result<()> {
        let output = self.run(Operation::Delete, account)?;
        if output.status.code() == Some(44) {
            return Ok(());
        }
        check_status(&output, "delete")
    }
}

impl Keychain {
    fn run(&self, op: Operation<'_>, account: &str) -> Result<Output> {
        let (argv, input) = keychain_argv(op, &self.service, account)?;
        let mut command = Command::new("/usr/bin/security");
        command.args(argv).env("LC_ALL", "C").stderr(Stdio::null());
        let Some(input) = input else {
            return command.output().context("run macOS security");
        };
        // Suppress interactive output. Neither command diagnostics nor the line sent to
        // the interpreter should become part of a caller's error or log.
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .context("start macOS security")?;
        let write = child.stdin.take().unwrap().write_all(input.as_bytes());
        let output = child
            .wait_with_output()
            .context("wait for macOS security")?;
        if output.status.success() {
            write.context("write macOS security input")?;
        }
        Ok(output)
    }
}

fn check_status(output: &Output, action: &str) -> Result<()> {
    if !output.status.success() {
        bail!("macOS Keychain {action} failed ({})", output.status);
    }
    Ok(())
}

enum Operation<'a> {
    Get,
    Set(&'a str),
    Delete,
}

fn keychain_argv(
    op: Operation<'_>,
    service: &str,
    account: &str,
) -> Result<(Vec<String>, Option<String>)> {
    let command = match op {
        Operation::Get => "find-generic-password",
        Operation::Delete => "delete-generic-password",
        Operation::Set(token) => {
            ensure!(valid_token(token), "invalid plugin token");
            for name in [service, account] {
                ensure!(
                    !name.is_empty()
                        && !name.contains(['"', '\\'])
                        && !name.chars().any(char::is_control),
                    "invalid macOS Keychain service or account"
                );
            }
            return Ok((
                vec!["-i".to_owned()],
                Some(format!(
                    "add-generic-password -U -s \"{service}\" -a \"{account}\" -w \"{token}\"\n"
                )),
            ));
        }
    };
    let mut argv: Vec<_> = [command, "-s", service, "-a", account]
        .into_iter()
        .map(str::to_owned)
        .collect();
    if matches!(op, Operation::Get) {
        argv.push("-w".to_owned());
    }
    Ok((argv, None))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_token_validation() {
        for token in [
            "ghp_xyz0",
            "github_pat_abc.def",
            "glpat-123234",
            &"a".repeat(512),
        ] {
            assert!(valid_token(token));
        }
        for token in [
            "",
            "abc1234",
            "token with space",
            "token\"quote",
            "token\\escape",
            "token\nnewline",
            "notASCII中文",
            &"a".repeat(513),
        ] {
            assert!(!valid_token(token));
        }
    }

    #[test]
    fn secret_memory_reads_updates_and_deletes_by_plugin() -> Result<()> {
        let memory = Memory::default();
        assert_eq!(memory.get("github")?, None);
        memory.set("github", "ghp_first")?;
        memory.set("gitlab", "glpat-first")?;
        assert!(memory.set("github", "invalid").is_err());
        assert_eq!(memory.get("github")?.as_deref(), Some("ghp_first"));
        memory.set("github", "ghp_updated")?;
        assert_eq!(memory.get("github")?.as_deref(), Some("ghp_updated"));
        memory.delete("github")?;
        memory.delete("github")?;
        assert_eq!(memory.get("github")?, None);
        assert_eq!(memory.get("gitlab")?.as_deref(), Some("glpat-first"));
        Ok(())
    }

    #[test]
    fn secret_keychain_arguments_keep_password_out_of_process_args() -> Result<()> {
        let service = Keychain::default().service;
        let token = "ghp_keychain_fixture";
        let (args, input) = keychain_argv(Operation::Set(token), &service, "github")?;
        assert_eq!(args, ["-i"]);
        assert_eq!(
            input.as_deref(),
            Some(
                "add-generic-password -U -s \"AEP Studio plugin token\" -a \"github\" -w \"ghp_keychain_fixture\"\n"
            )
        );
        let (args, input) = keychain_argv(Operation::Get, &service, "github")?;
        assert_eq!(
            args,
            [
                "find-generic-password",
                "-s",
                &service,
                "-a",
                "github",
                "-w"
            ]
        );
        assert!(input.is_none());
        let (args, input) = keychain_argv(Operation::Delete, &service, "github")?;
        assert_eq!(
            args,
            ["delete-generic-password", "-s", &service, "-a", "github"]
        );
        assert!(input.is_none());
        assert!(keychain_argv(Operation::Set(token), &service, "bad\naccount").is_err());
        assert!(keychain_argv(Operation::Set(token), "bad\"service", "github").is_err());
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires an authorized macOS login keychain"]
    fn secret_keychain_roundtrip() -> Result<()> {
        let keychain = Keychain {
            service: format!("AEP Studio T2 test {}", uuid::Uuid::new_v4()),
        };
        let roundtrip = || -> Result<()> {
            assert_eq!(keychain.get("github")?, None);
            keychain.set("github", "ghp_keychain_fixture")?;
            ensure!(
                keychain.get("github")?.as_deref() == Some("ghp_keychain_fixture"),
                "keychain roundtrip returned a different value"
            );
            keychain.set("github", "ghp_keychain_replaced")?;
            ensure!(
                keychain.get("github")?.as_deref() == Some("ghp_keychain_replaced"),
                "keychain update returned a different value"
            );
            Ok(())
        };
        let result = roundtrip();
        let cleanup = keychain.delete("github");
        result?;
        cleanup?;
        keychain.delete("github")?;
        assert_eq!(keychain.get("github")?, None);
        Ok(())
    }
}
