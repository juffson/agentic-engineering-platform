use std::collections::HashSet;

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
        let mut seen_paths = HashSet::new();
        for path in &self.allowed_paths {
            ensure!(
                seen_paths.insert(path),
                "duplicate allowed_paths entry: {path}"
            );
        }
        ensure!(
            !self.check.is_empty() && self.check.iter().all(|part| !part.is_empty()),
            "check must contain a command"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::TaskManifest;

    #[test]
    fn rejects_duplicate_allowed_path() {
        let manifest = TaskManifest {
            title: None,
            allowed_paths: vec![
                "src/task.rs".into(),
                "src/lib.rs".into(),
                "src/task.rs".into(),
            ],
            check: vec!["cargo".into(), "test".into()],
        };

        assert_eq!(
            manifest.validate().unwrap_err().to_string(),
            "duplicate allowed_paths entry: src/task.rs"
        );
    }

    #[test]
    fn accepts_distinct_valid_paths() {
        let manifest = TaskManifest {
            title: None,
            allowed_paths: vec!["src/task.rs".into(), "src/lib.rs".into()],
            check: vec!["cargo".into(), "test".into()],
        };

        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn accepts_paths_with_different_case() {
        let manifest = TaskManifest {
            title: None,
            allowed_paths: vec!["src/task.rs".into(), "src/Task.rs".into()],
            check: vec!["cargo".into(), "test".into()],
        };

        assert!(manifest.validate().is_ok());
    }
}
