use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    Revoke {
        old: String,
        replacement: String,
    },
    Publish {
        target: String,
        candidate: String,
    },
    Merge {
        target: String,
        candidate: String,
        pr: u64,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Gate {
    pub id: String,
    pub worker: String,
    pub repository: String,
    #[serde(default)]
    pub binding: String,
    #[serde(flatten)]
    pub action: Action,
}

impl Gate {
    pub fn same_scope(&self, other: &Self) -> bool {
        self.worker == other.worker
            && self.repository == other.repository
            && self.binding == other.binding
            && self.action == other.action
    }

    pub fn receipt(&self, root: &Path) -> PathBuf {
        root.join("evidence/approvals")
            .join(format!("{}.json", self.id))
    }

    pub fn valid(&self) -> bool {
        if uuid::Uuid::parse_str(&self.id).is_err() {
            return false;
        }
        let delivery = |target: &str, candidate: &str| {
            target.starts_with("test/https-rolling-")
                && aep::workspace::ident(&target[5..], 63).is_ok()
                && candidate.len() == 40
                && candidate.bytes().all(|ch| ch.is_ascii_hexdigit())
        };
        match &self.action {
            Action::Revoke { old, replacement } => {
                old != replacement
                    && aep::deploy_keys::fingerprint(old)
                    && aep::deploy_keys::fingerprint(replacement)
            }
            Action::Publish { target, candidate } => delivery(target, candidate),
            Action::Merge {
                target,
                candidate,
                pr,
            } => *pr > 0 && delivery(target, candidate),
        }
    }
}
