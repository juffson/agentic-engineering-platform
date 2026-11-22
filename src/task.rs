use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fs, path::Path};

/// Files an implementation may change and the argument vector used for acceptance.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct TaskManifest {
    pub title: Option<String>,
    pub allowed_paths: Vec<String>,
    pub check: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub criteria: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<Check>,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub id: String,
    pub criteria: Vec<String>,
    pub command: Vec<String>,
}

#[derive(Deserialize, Serialize)]
pub struct TaskDraft {
    pub name: String,
    pub spec: String,
    pub manifest: TaskManifest,
}

impl TaskDraft {
    pub fn validate(&self) -> Result<()> {
        crate::workspace::ident(&self.name, 63)
            .context("任务 ID 只支持小写字母、数字和连字符，最多 63 个字符")?;
        ensure!(!self.spec.trim().is_empty(), "请填写 Spec");
        self.manifest
            .validate()
            .context("请检查允许修改的文件和验收命令")
    }

    pub fn write_files(&self, dir: &Path) -> Result<()> {
        self.validate()?;
        fs::create_dir(dir).context("任务目录已存在，或无法创建任务目录")?;
        fs::write(dir.join("spec.md"), &self.spec)?;
        let file = tempfile::NamedTempFile::new_in(dir)?;
        serde_json::to_writer_pretty(file.as_file(), &self.manifest)?;
        file.persist(dir.join("task.json"))?;
        Ok(())
    }
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
        ensure!(!self.allowed_paths.is_empty(), "invalid allowed_paths");
        for (index, path) in self.allowed_paths.iter().enumerate() {
            let entry = index + 1;
            ensure!(
                !path.contains('\0'),
                "invalid allowed_paths entry #{entry}: contains NUL"
            );
            ensure!(
                path.split('/')
                    .all(|part| !matches!(part, "" | "." | ".." | ".git")),
                "invalid allowed_paths entry #{entry}: invalid segment"
            );
        }
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

    fn manifest_with_paths(allowed_paths: &[&str]) -> TaskManifest {
        TaskManifest {
            title: None,
            allowed_paths: allowed_paths.iter().map(|path| path.to_string()).collect(),
            check: vec!["true".into()],
            criteria: vec![],
            checks: vec![],
        }
    }

    #[test]
    fn rejects_nul_anywhere_in_allowed_paths() {
        for (paths, entry) in [
            (&["\0"][..], 1),
            (&["\0src/task.rs"][..], 1),
            (&["src/\0task.rs"][..], 1),
            (&["src/task.rs\0"][..], 1),
            (&["src/task.rs", "docs/\0plan.md"][..], 2),
            (&["src/task.rs", "docs/plan.md", "a\0b.rs"][..], 3),
        ] {
            let error = manifest_with_paths(paths)
                .validate()
                .expect_err("NUL in allowed_paths must be rejected before file or Git operations");
            let message = error.to_string();
            assert!(
                message.starts_with("invalid allowed_paths"),
                "unexpected diagnostic: {error}"
            );
            assert!(
                message.contains(&format!("entry #{entry}")),
                "missing 1-based position: {error}"
            );
            assert!(
                !message.contains('\0'),
                "diagnostic must not echo the raw path: {error}"
            );
        }
    }

    #[test]
    fn keeps_existing_allowed_paths_rules() {
        assert!(
            manifest_with_paths(&["src/my notes/task file.rs"])
                .validate()
                .is_ok()
        );
        let error = manifest_with_paths(&[])
            .validate()
            .expect_err("empty allowed_paths must stay invalid");
        assert_eq!(error.to_string(), "invalid allowed_paths");
        for (paths, entry) in [
            (&["src//task.rs"][..], 1),
            (&["src/./task.rs"][..], 1),
            (&["src/../task.rs"][..], 1),
            (&[".git/config"][..], 1),
            (&["src/task.rs", "docs/plan.md", ".git/config"][..], 3),
            (&["src/task.rs", "docs/plan.md", "other.rs", ""][..], 4),
        ] {
            let error = manifest_with_paths(paths)
                .validate()
                .expect_err("existing allowed_paths rules must keep rejecting these");
            let message = error.to_string();
            assert!(
                message.starts_with("invalid allowed_paths"),
                "unexpected diagnostic: {error}"
            );
            assert!(
                message.contains(&format!("entry #{entry}")),
                "missing 1-based position: {error}"
            );
            assert!(
                !message.contains(".git") && !message.contains("src"),
                "diagnostic must not echo the raw path: {error}"
            );
        }
    }

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
