use anyhow::{Context, Result};
use std::{
    env, fs,
    path::{Path, PathBuf},
};

pub(crate) fn storage() -> Option<PathBuf> {
    env::home_dir().map(|home| home.join("Library/Application Support/AEP Studio"))
}

pub(crate) fn last_path() -> Option<PathBuf> {
    serde_json::from_slice(&fs::read(storage()?.join("repository.json")).ok()?).ok()
}

pub(crate) fn remember(path: &Path) -> Result<()> {
    let dir = storage().context("找不到 AEP Studio 的设置目录")?;
    fs::create_dir_all(&dir)?;
    crate::lifecycle::save(&dir, "repository.json", &path)
}
