//! A cooperative stop request addressed to an exact local Run ID.
use crate::workspace::{ident, read};
use anyhow::{Result, ensure};
use std::{
    fs::{self, File, OpenOptions},
    io::ErrorKind,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("run cancelled by the operator")
    }
}

impl std::error::Error for Cancelled {}

pub struct Cancellation {
    dir: PathBuf,
    evidence: PathBuf,
    run: String,
}

impl Cancellation {
    pub fn new(data: &Path, run: &str) -> Result<Self> {
        ident(run, 64)?;
        Ok(Self {
            dir: data.join("run-control"),
            evidence: data.join("runs").join(run).join("evidence.json"),
            run: run.into(),
        })
    }

    fn marker(&self) -> PathBuf {
        self.dir.join(format!("{}.cancel", self.run))
    }

    pub fn requested(&self) -> Result<bool> {
        Ok(self.marker().try_exists()?)
    }

    pub fn check(&self) -> Result<()> {
        ensure!(!self.requested()?, Cancelled);
        Ok(())
    }

    /// Serializes a cancellation request with the executor's terminal evidence write.
    pub fn lock(&self) -> Result<File> {
        fs::create_dir_all(&self.dir)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.dir.join(format!("{}.lock", self.run)))?;
        lock.lock()?;
        Ok(lock)
    }

    /// Returns false when the executor already recorded its final result.
    pub fn request(&self) -> Result<bool> {
        let _lock = self.lock()?;
        if self.evidence.try_exists()? && read(&self.evidence)?["status"] != "running" {
            return Ok(false);
        }
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.marker())
        {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(true),
            Err(error) => Err(error.into()),
        }
    }

    /// The queue holder may reuse a reserved ID if the previous worker never launched it.
    pub fn clear_unlaunched(&self) -> Result<()> {
        if self.requested()? {
            let _lock = self.lock()?;
            ensure!(
                !self.evidence.try_exists()?,
                "run already has execution evidence"
            );
            fs::remove_file(self.marker())?;
        }
        Ok(())
    }
}
