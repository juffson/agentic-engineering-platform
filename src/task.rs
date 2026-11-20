use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Files an implementation may change and the argument vector used for acceptance.
#[derive(Clone, Deserialize, Serialize)]
pub struct TaskManifest {
    pub title: Option<String>,
    pub allowed_paths: Vec<String>,
    pub check: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub criteria: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<Check>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub id: String,
    pub criteria: Vec<String>,
    pub command: Vec<String>,
}

fn valid_command(command: &[String]) -> bool {
    command
        .first()
        .is_some_and(|executable| !executable.is_empty())
        && command.iter().all(|part| !part.contains('\0'))
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

impl TaskManifest {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.allowed_paths.is_empty()
                && self.allowed_paths.iter().all(|path| {
                    path.split('/')
                        .all(|part| !matches!(part, "" | "." | ".." | ".git"))
                }),
            "invalid allowed_paths"
        );
        ensure!(
            self.check
                .first()
                .is_some_and(|executable| !executable.is_empty()),
            "check must contain a command"
        );
        ensure!(
            valid_command(&self.check),
            "acceptance argv cannot contain NUL bytes"
        );
        ensure!(
            self.criteria.len() <= 32 && self.checks.len() <= 16,
            "acceptance plan is too large"
        );
        let mut ids = BTreeSet::from(["primary"]);
        for check in &self.checks {
            ensure!(
                valid_id(&check.id) && ids.insert(&check.id),
                "invalid or duplicate acceptance check ID"
            );
            ensure!(
                valid_command(&check.command),
                "invalid command for {}",
                check.id
            );
            ensure!(
                !check.criteria.is_empty(),
                "{} must identify its acceptance criteria",
                check.id
            );
        }
        ensure!(
            self.criteria
                .iter()
                .chain(self.checks.iter().flat_map(|check| &check.criteria))
                .all(|id| valid_id(id)),
            "invalid acceptance criterion ID"
        );
        Ok(())
    }

    pub fn criterion_ids(&self) -> Vec<&str> {
        self.criteria
            .iter()
            .chain(self.checks.iter().flat_map(|check| &check.criteria))
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(command: &[&str], additional: bool) -> TaskManifest {
        let command: Vec<String> = command.iter().map(|part| (*part).to_owned()).collect();
        let mut manifest = TaskManifest {
            title: None,
            allowed_paths: vec!["src/task.rs".into()],
            check: vec!["true".into()],
            criteria: vec![],
            checks: vec![],
        };
        if additional {
            manifest.checks.push(Check {
                id: "extra".into(),
                criteria: vec!["AC-1".into()],
                command,
            });
        } else {
            manifest.check = command;
        }
        manifest
    }

    #[test]
    fn acceptance_commands_preserve_empty_arguments() {
        for additional in [false, true] {
            for command in [vec!["sh"], vec!["sh", ""], vec!["sh", "", "arg", ""]] {
                assert!(manifest(&command, additional).validate().is_ok());
            }
        }
    }

    #[test]
    fn acceptance_commands_require_an_executable_and_reject_nul() {
        for additional in [false, true] {
            for command in [
                vec![],
                vec![""],
                vec!["", "arg"],
                vec!["sh\0"],
                vec!["sh", "\0"],
                vec!["sh", "", "arg\0"],
            ] {
                assert!(manifest(&command, additional).validate().is_err());
            }
        }
    }
}
