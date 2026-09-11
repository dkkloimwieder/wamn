//! Temporary directory ownership shared by route tests.

use std::path::{Path, PathBuf};

use anyhow::Context as _;

#[derive(Debug)]
pub struct ScratchRoot(pub PathBuf);

impl ScratchRoot {
    pub fn create() -> anyhow::Result<Self> {
        let path = scratch_root();
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).context("create route-auth proof directory")?;
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

fn scratch_root() -> PathBuf {
    std::env::temp_dir().join(format!("route-authentication-live-{}", std::process::id()))
}
