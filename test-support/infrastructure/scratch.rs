//! Temporary directory ownership shared by route tests.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context as _;

#[derive(Debug)]
pub struct ScratchRoot(pub PathBuf);

impl ScratchRoot {
    pub fn create() -> anyhow::Result<Self> {
        let path = scratch_root();
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).context("create route-auth test directory")?;
        Ok(Self(path))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Distinguishes the directories that one process creates, so that concurrent
/// tests never remove or recreate a directory that another test still uses.
static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

fn scratch_root() -> PathBuf {
    std::env::temp_dir().join(format!(
        "route-authentication-live-{}-{}",
        std::process::id(),
        NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed)
    ))
}
