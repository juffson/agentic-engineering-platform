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
            !self.check[0].trim().is_empty(),
            "acceptance requires an executable"
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
    fn rejects_whitespace_only_executable() {
        for program in [" ", "\t", "\n\r", " \t\n\r ", "\u{2003}"] {
            let error = manifest(&[program, "test"]).validate().unwrap_err();
            assert_eq!(error.to_string(), "acceptance requires an executable");
        }
    }

    #[test]
    fn rejects_empty_arguments() {
        for check in [&[][..], &[""][..], &["cargo", ""][..], &["", "test"][..]] {
            let error = manifest(check).validate().unwrap_err();
            assert_eq!(error.to_string(), "check must contain a command");
        }
    }

    #[test]
    fn preserves_argument_whitespace() {
        for program in [" cargo", "cargo ", "cargo test", "\tcargo\n"] {
            let manifest = manifest(&[
                program,
                " leading",
                "trailing ",
                "internal whitespace",
                " ",
                "\t\r\n",
            ]);
            let original = manifest.check.clone();

            manifest.validate().unwrap();

            assert_eq!(manifest.check, original);
        }
    }
}
