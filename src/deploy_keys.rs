//! Public metadata exchanged by Studio, the control plane and a Worker.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Key {
    pub public: String,
    pub fingerprint: String,
}

pub fn fingerprint(value: &str) -> bool {
    value.strip_prefix("SHA256:").is_some_and(|hash| {
        hash.len() == 43
            && hash
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"+/".contains(&c))
    })
}

impl Key {
    pub fn validate(&self) -> Result<()> {
        let mut words = self.public.split(' ');
        let algorithm = words.next().unwrap_or_default();
        let blob = words.next().unwrap_or_default();
        ensure!(
            matches!(
                algorithm,
                "ssh-ed25519"
                    | "ssh-rsa"
                    | "ecdsa-sha2-nistp256"
                    | "ecdsa-sha2-nistp384"
                    | "ecdsa-sha2-nistp521"
            ) && (32..=2048).contains(&blob.len())
                && blob
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"+/=".contains(&c))
                && words.next().is_none()
                && fingerprint(&self.fingerprint),
            "invalid Deploy Key metadata"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    pub active: Option<Key>,
    pub staged: Option<Key>,
    pub retiring: Option<Key>,
}

impl Inventory {
    pub fn validate(&self) -> Result<()> {
        let mut known = std::collections::HashSet::new();
        for key in [&self.active, &self.staged, &self.retiring]
            .into_iter()
            .flatten()
        {
            key.validate()?;
            ensure!(known.insert(&key.fingerprint), "duplicate Deploy Key slots");
        }
        ensure!(
            self.retiring.is_none() || (self.active.is_some() && self.staged.is_none()),
            "invalid Deploy Key slots"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Change {
    Prepare,
    Activate { fingerprint: String },
    Verify { fingerprint: String },
    Forget { fingerprint: String },
}

impl Change {
    pub fn validate(&self, inventory: &Inventory) -> Result<()> {
        match self {
            Self::Prepare => ensure!(
                inventory.staged.is_none() && inventory.retiring.is_none(),
                "finish the current Deploy Key rotation first"
            ),
            Self::Activate { fingerprint } => ensure!(
                inventory
                    .staged
                    .as_ref()
                    .is_some_and(|key| key.fingerprint == *fingerprint)
                    && inventory.retiring.is_none(),
                "the staged Deploy Key changed"
            ),
            Self::Verify { fingerprint } => ensure!(
                inventory
                    .active
                    .as_ref()
                    .is_some_and(|key| key.fingerprint == *fingerprint),
                "the active Deploy Key changed"
            ),
            Self::Forget { fingerprint } => ensure!(
                inventory
                    .retiring
                    .as_ref()
                    .is_some_and(|key| key.fingerprint == *fingerprint),
                "the retiring Deploy Key changed"
            ),
        }
        Ok(())
    }

    pub fn verifies_active(&self, inventory: &Inventory) -> bool {
        let (Self::Activate { fingerprint } | Self::Verify { fingerprint }) = self else {
            return false;
        };
        inventory
            .active
            .as_ref()
            .is_some_and(|key| key.fingerprint == *fingerprint)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: String,
    pub change: Change,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub id: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerKeys {
    pub repository: String,
    pub installation: String,
    pub inventory: Inventory,
    pub request: Option<Request>,
    pub last: Option<Receipt>,
    pub verified_at: Option<u64>,
}
