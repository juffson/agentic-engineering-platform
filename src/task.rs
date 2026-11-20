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
    command.first().is_some_and(|part| !part.is_empty())
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
            self.check.first().is_some_and(|part| !part.is_empty()),
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

    #[test]
    fn acceptance_commands_preserve_empty_arguments_but_require_an_executable() {
        for (argv, valid) in [
            (vec!["sh", "-c", "test -z \"$1\"", "sh", ""], true),
            (vec!["command", "", "argument", ""], true),
            (vec!["command"], true),
            (vec![], false),
            (vec![""], false),
            (vec!["", "argument"], false),
            (vec!["com\0mand"], false),
            (vec!["command", "arg\0ument"], false),
            (vec!["command", "", "\0"], false),
        ] {
            let command: Vec<String> = argv.iter().map(|part| (*part).into()).collect();
            let mut manifest = TaskManifest {
                title: None,
                allowed_paths: vec!["src/task.rs".into()],
                check: command.clone(),
                criteria: vec!["AC-1".into()],
                checks: vec![],
            };
            assert_eq!(manifest.validate().is_ok(), valid, "primary: {argv:?}");

            manifest.check = vec!["command".into()];
            manifest.checks.push(Check {
                id: "additional".into(),
                criteria: vec!["AC-2".into()],
                command,
            });
            assert_eq!(manifest.validate().is_ok(), valid, "additional: {argv:?}");
        }
    }
}
