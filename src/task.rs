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
    !command.is_empty()
        && !command[0].trim().is_empty()
        && command
            .iter()
            .all(|part| !part.is_empty() && !part.contains('\0'))
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
            !self.check.is_empty() && self.check.iter().all(|part| !part.is_empty()),
            "check must contain a command"
        );
        ensure!(
            valid_command(&self.check),
            "invalid acceptance command: executable is blank or an argument contains NUL bytes"
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
    fn validates_primary_and_additional_command_arguments() {
        for command in [
            vec![],
            vec![""],
            vec![" \t"],
            vec!["\n\r"],
            vec!["\u{2003}"],
            vec!["tool", ""],
            vec!["tool\0"],
            vec!["tool", "arg\0"],
            vec![" /path with spaces/tool ", " \t", "argument"],
        ] {
            let valid = command == [" /path with spaces/tool ", " \t", "argument"];
            let command: Vec<String> = command.into_iter().map(String::from).collect();
            let mut task = TaskManifest {
                title: None,
                allowed_paths: vec!["src/task.rs".into()],
                check: command.clone(),
                criteria: vec![],
                checks: vec![],
            };
            assert_eq!(task.validate().is_ok(), valid, "primary: {command:?}");
            assert_eq!(task.check, command);

            task.check = vec!["tool".into()];
            task.checks.push(Check {
                id: "extra".into(),
                criteria: vec!["AC-1".into()],
                command: command.clone(),
            });
            let result = task.validate();
            assert_eq!(result.is_ok(), valid, "additional: {command:?}");
            if let Err(error) = result {
                assert!(error.to_string().contains("extra"));
            }
            assert_eq!(task.checks[0].command, command);
        }
    }
}
