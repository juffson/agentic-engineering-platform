use super::{Site, Status, archive_path, running, sessions_dir};
use crate::https_pilot::read_json;
use aep::workspace::Workspace;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct Selection {
    entry: Option<String>,
    site: Option<String>,
    worker: Option<String>,
}

pub(crate) struct Entry {
    pub id: String,
    pub root: PathBuf,
    pub site: Site,
    pub status: Status,
    pub active: bool,
    pub archived: bool,
}

pub(crate) struct History {
    pub source: PathBuf,
    directory: PathBuf,
    selection: Selection,
    entries: Vec<Entry>,
    refreshed: Option<Instant>,
}

impl History {
    pub fn load(source: &Workspace) -> Self {
        let directory = sessions_dir(source);
        let mut history = Self {
            source: source.root.clone(),
            selection: read_json(directory.join("history.json")).unwrap_or_default(),
            directory,
            entries: Vec::new(),
            refreshed: None,
        };
        history.refresh(true);
        history
    }

    pub fn refresh(&mut self, force: bool) {
        if !force
            && self
                .refreshed
                .is_some_and(|time| time.elapsed() < Duration::from_secs(1))
        {
            return;
        }
        let mut roots: Vec<_> = fs::read_dir(&self.directory)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .collect();
        roots.sort_by_cached_key(|entry| {
            let time = entry
                .metadata()
                .and_then(|meta| meta.created().or_else(|_| meta.modified()))
                .ok();
            std::cmp::Reverse((time, entry.file_name()))
        });
        self.entries = roots
            .into_iter()
            .filter_map(|entry| {
                let id = entry.file_name().into_string().ok()?;
                if !id.strip_prefix("studio-").is_some_and(|suffix| {
                    !suffix.is_empty()
                        && id.len() <= 63
                        && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
                }) {
                    return None;
                }
                let root = entry.path();
                Some(Entry {
                    id,
                    site: read_json(root.join("site.json"))?,
                    status: read_json(root.join("rolling-state.json")).unwrap_or_default(),
                    active: running(&root),
                    archived: archive_path(&root).is_some(),
                    root,
                })
            })
            .collect();
        self.refreshed = Some(Instant::now());
    }

    pub fn preferred(&self) -> Option<&Path> {
        self.selection
            .entry
            .as_ref()
            .and_then(|id| self.entries.iter().find(|entry| &entry.id == id))
            .or_else(|| self.entries.iter().find(|entry| entry.active))
            .or_else(|| self.entries.first())
            .map(|entry| entry.root.as_path())
    }

    pub fn lookup(&self, root: &Path) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.root == root)
    }

    pub fn any_active(&self) -> bool {
        self.entries.iter().any(|entry| entry.active)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn site(&self) -> Option<&str> {
        self.selection.site.as_deref()
    }

    pub fn worker(&self) -> Option<&str> {
        self.selection.worker.as_deref()
    }

    pub fn sites(&self) -> BTreeSet<&str> {
        self.entries
            .iter()
            .map(|entry| entry.site.name.as_str())
            .collect()
    }

    pub fn workers(&self) -> BTreeSet<&str> {
        self.entries
            .iter()
            .filter(|entry| self.at_site(entry))
            .flat_map(|entry| entry.site.workers.iter().map(String::as_str))
            .collect()
    }

    pub fn visible(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|entry| {
            self.at_site(entry)
                && self
                    .worker()
                    .is_none_or(|worker| entry.site.workers.iter().any(|id| id == worker))
        })
    }

    fn at_site(&self, entry: &Entry) -> bool {
        self.site().is_none_or(|site| entry.site.name == site)
    }

    pub fn select(&mut self, root: &Path) -> Result<()> {
        let entry = self.lookup(root).context("找不到演练记录")?;
        let id = entry.id.clone();
        let in_site = self.at_site(entry);
        let in_worker = self
            .worker()
            .is_none_or(|worker| entry.site.workers.iter().any(|id| id == worker));
        self.selection.entry = Some(id);
        if !in_site {
            self.selection.site = None;
        }
        if !in_worker {
            self.selection.worker = None;
        }
        self.save()
    }

    pub fn filter_site(&mut self, site: Option<String>) -> Result<()> {
        self.selection.site = site;
        if self
            .worker()
            .is_some_and(|worker| !self.workers().contains(worker))
        {
            self.selection.worker = None;
        }
        self.save()
    }

    pub fn filter_worker(&mut self, worker: Option<String>) -> Result<()> {
        self.selection.worker = worker;
        self.save()
    }

    fn save(&self) -> Result<()> {
        fs::create_dir_all(&self.directory)?;
        let temp = tempfile::NamedTempFile::new_in(&self.directory)?;
        serde_json::to_writer(temp.as_file(), &self.selection)?;
        temp.persist(self.directory.join("history.json"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rolling::Session;
    use std::fs::File;

    #[test]
    fn filters_and_selection_survive_reopen_and_keep_receipts_scoped() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let ws = Workspace {
            root: temp.path().into(),
            data: temp.path().join("data"),
        };
        let directory = sessions_dir(&ws);
        let fixture = |id: &str, site: &str, workers: &[&str], exit: i32| -> Result<PathBuf> {
            let path = directory.join(id);
            fs::create_dir_all(&path)?;
            let site = Site {
                name: site.into(),
                endpoint: "https://127.0.0.1:18443".into(),
                access: temp.path().into(),
                repository: "owner/private".into(),
                workers: workers.iter().map(|worker| (*worker).to_owned()).collect(),
            };
            serde_json::to_writer(File::create(path.join("site.json"))?, &site)?;
            serde_json::to_writer(
                File::create(path.join("rolling-state.json"))?,
                &serde_json::json!({"stage":"dispatch-during-failure","exit_code":exit}),
            )?;
            Ok(path)
        };
        let earlier = fixture("studio-earlier", "private", &["alpha", "beta"], 0)?;
        let paused = fixture("studio-PauseD", "private", &["alpha", "gamma"], 4)?;
        let elsewhere = fixture("studio-elsewhere", "other", &["gamma", "delta"], 1)?;
        File::create(earlier.join("evidence.tar.gz"))?;
        File::create(paused.join("owner.lock"))?;

        let attempts = paused.join("evidence/attempts/attempt-one");
        fs::create_dir_all(&attempts)?;
        serde_json::to_writer(
            File::create(attempts.join("result.json"))?,
            &serde_json::json!({"stage":"preflight","exit_code":1,"reason":"lost connection"}),
        )?;

        let mut history = History::load(&ws);
        assert_eq!(history.visible().count(), 3);
        history.filter_site(Some("private".into()))?;
        history.filter_worker(Some("gamma".into()))?;
        assert_eq!(
            history
                .visible()
                .map(|entry| &entry.root)
                .collect::<Vec<_>>(),
            [&paused]
        );
        history.select(&paused)?;

        let mut reopened = History::load(&ws);
        assert_eq!(reopened.site(), Some("private"));
        assert_eq!(reopened.worker(), Some("gamma"));
        assert_eq!(reopened.preferred(), Some(paused.as_path()));
        let selected = Session::open(&ws, paused).unwrap();
        assert!(selected.can_resume());
        assert_eq!(selected.attempts[0].status.reason, "lost connection");
        assert!(
            Session::open(&ws, earlier.clone())
                .unwrap()
                .attempts
                .is_empty()
        );
        assert!(reopened.lookup(&earlier).unwrap().archived);

        reopened.filter_site(Some("other".into()))?;
        assert_eq!(
            reopened
                .visible()
                .map(|entry| &entry.root)
                .collect::<Vec<_>>(),
            [&elsewhere]
        );
        reopened.filter_worker(Some("delta".into()))?;
        reopened.filter_site(Some("private".into()))?;
        assert_eq!(reopened.worker(), None);
        assert_eq!(reopened.visible().count(), 2);
        assert!(Session::open(&ws, directory.join("../studio-outside")).is_none());
        Ok(())
    }
}
