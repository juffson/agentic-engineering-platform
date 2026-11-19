use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

/// Files an implementation may change and the argument vector used for acceptance.
#[derive(Clone, Deserialize, Serialize)]
pub struct TaskManifest {
    pub title: Option<String>,
    pub allowed_paths: Vec<String>,
    pub check: Vec<String>,
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
            self.check.iter().all(|part| !part.contains('\0')),
            "acceptance argv cannot contain NUL bytes"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::TaskManifest;

    fn manifest(check: &[&str]) -> TaskManifest {
        TaskManifest {
            title: None,
            allowed_paths: vec!["src/task.rs".into()],
            check: check.iter().map(|part| (*part).into()).collect(),
        }
    }

    #[test]
    fn rejects_nul_bytes_in_any_acceptance_argument() {
        for check in [
            vec!["\0tool", "arg"],
            vec!["to\0ol", "arg"],
            vec!["tool\0", "arg"],
            vec!["tool", "\0"],
            vec!["tool", "arg\0ument", "last"],
            vec!["tool", "arg", "last\0"],
        ] {
            assert_eq!(
                manifest(&check).validate().unwrap_err().to_string(),
                "acceptance argv cannot contain NUL bytes",
                "check: {check:?}"
            );
        }
    }

    #[test]
    fn preserves_acceptance_arguments_exactly() {
        let task = manifest(&[
            "  executable\t",
            " ",
            "\t\n",
            "  two words  ",
            "雪🦀",
            "--flag=value",
            "literal\\0",
        ]);
        let original = task.check.clone();

        task.validate().unwrap();

        assert_eq!(task.check, original);
    }

    #[test]
    fn rejects_empty_commands_and_arguments() {
        for check in [vec![], vec![""], vec!["", "arg"], vec!["tool", ""]] {
            assert_eq!(
                manifest(&check).validate().unwrap_err().to_string(),
                "check must contain a command",
                "check: {check:?}"
            );
        }
    }

    #[test]
    fn rejects_invalid_allowed_paths() {
        for paths in [
            vec![],
            vec![""],
            vec!["."],
            vec![".."],
            vec![".git"],
            vec!["/src"],
            vec!["src/"],
            vec!["src//task.rs"],
            vec!["src/./task.rs"],
            vec!["src/../task.rs"],
            vec!["src/.git/config"],
        ] {
            let mut task = manifest(&["tool", "arg"]);
            task.allowed_paths = paths.iter().map(|path| (*path).into()).collect();

            assert_eq!(
                task.validate().unwrap_err().to_string(),
                "invalid allowed_paths",
                "paths: {paths:?}"
            );
        }
    }
}
