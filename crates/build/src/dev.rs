//! Watch package inputs and repeat the product deployment operation.

#[cfg(target_os = "linux")]
pub mod development;
pub mod edge_bundle;

use std::path::{Path, PathBuf};

use wamn_digest::Digest;
use wamn_registry::ApplicationName;

#[cfg(target_os = "linux")]
use crate::watch::FilesystemInvalidationSource;

/// A filesystem event's effect on development output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DevInvalidation {
    Ignore,
    Rerun,
}

/// Paths the next development pass needs to observe.
#[derive(Debug)]
pub struct WatchInputs {
    pub roots: Vec<PathBuf>,
    pub shared: Vec<PathBuf>,
    pub files: Vec<PathBuf>,
}

/// Deploy a published digest through the application deployment entry point.
pub trait Deploy: Sync {
    fn deploy(
        &self,
        configuration: &Path,
        application: ApplicationName,
        release: Digest,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;
}

/// Build, publish and deploy once, then repeat for coalesced source changes.
/// Shutdown waits for an active deployment to finish. A failed watch pass is
/// reported and the next source change retries through the same product path.
#[cfg(target_os = "linux")]
pub async fn run(
    mut source: Option<FilesystemInvalidationSource>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    mut deploy: impl AsyncFnMut(&mut Option<FilesystemInvalidationSource>) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    loop {
        if *shutdown.borrow() {
            return Ok(());
        }
        let result = deploy(&mut source).await;
        let Some(source) = source.as_mut() else {
            return result;
        };
        if let Err(error) = result {
            tracing::error!(error = %error, "development deployment failed");
        }
        loop {
            let event = tokio::select! {
                biased;
                () = stopping(&mut shutdown) => return Ok(()),
                event = source.next() => event?,
            };
            let Some(event) = event else {
                return Ok(());
            };
            let mut changed = matches!(event, DevInvalidation::Rerun);
            while let Some(event) = source.try_next() {
                changed |= matches!(event, DevInvalidation::Rerun);
            }
            if changed {
                break;
            }
        }
    }
}

#[cfg(target_os = "linux")]
async fn stopping(shutdown: &mut tokio::sync::watch::Receiver<bool>) {
    while !*shutdown.borrow_and_update() {
        if shutdown.changed().await.is_err() {
            break;
        }
    }
}
