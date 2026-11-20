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
                    !path.contains('\0')
                        && path
                            .split('/')
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
    use super::TaskManifest;

    fn manifest(path: &str) -> TaskManifest {
        TaskManifest {
            title: None,
            allowed_paths: vec![path.to_owned()],
            check: vec!["true".to_owned()],
            criteria: vec![],
            checks: vec![],
        }
    }

    #[test]
    fn rejects_nul_in_allowed_paths() {
        for path in ["\0file.txt", "dir/fi\0le.txt", "file.txt\0"] {
            assert!(manifest(path).validate().is_err(), "accepted {path:?}");
        }
    }

    #[test]
    fn accepts_valid_allowed_paths() {
        for path in ["dir/nested/file.txt", "dir/file name.txt", "目录/café.txt"] {
            assert!(manifest(path).validate().is_ok(), "rejected {path:?}");
        }
    }

    #[test]
    fn rejects_invalid_path_components() {
        for path in [
            "",
            "/file",
            "dir/",
            "dir//file",
            "./file",
            "dir/../file",
            "dir/.git/config",
        ] {
            assert!(manifest(path).validate().is_err(), "accepted {path:?}");
        }
    }
}
