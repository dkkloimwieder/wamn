//! Observe when a child reads a configured input without inspecting its logs.

use std::fs::File;
use std::path::Path;
use std::time::Duration;

use anyhow::ensure;
use rustix::fs::{Mode, OFlags};
use tokio::process::Child;

/// Create an input whose reader can be observed before any bytes arrive.
pub fn create(path: &Path) -> anyhow::Result<()> {
    rustix::fs::mkfifoat(rustix::fs::CWD, path, Mode::RUSR | Mode::WUSR)?;
    Ok(())
}

/// Return a writer when the child reads the input, or `None` when it exits.
/// Keep the writer open while inspecting the child, then drop it to end the read.
pub async fn reader(path: &Path, child: &mut Child) -> anyhow::Result<Option<File>> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match rustix::fs::open(path, OFlags::WRONLY | OFlags::NONBLOCK, Mode::empty()) {
            Ok(writer) => return Ok(Some(writer.into())),
            Err(rustix::io::Errno::NXIO) => {}
            Err(error) => return Err(error.into()),
        }
        if child.try_wait()?.is_some() {
            return Ok(None);
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "child did not read its configured input"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
