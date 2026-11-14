//! Watch package inputs and repeat the product deployment operation.

pub mod edge_bundle;
mod git_source;
#[cfg(target_os = "linux")]
pub mod watch;

use std::error::Error;

/// A filesystem event's effect on development output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DevInvalidation {
    Ignore,
    Rerun,
}

/// Receives filesystem changes without executing deployment work.
pub trait DevInvalidationSource {
    /// Failure to receive a source change.
    type Error: Error + Send + Sync + 'static;
    /// Wait for a source event or the end of the source.
    fn next(&mut self)
    -> impl Future<Output = Result<Option<DevInvalidation>, Self::Error>> + Send;
    /// Receive an event that is already queued.
    fn try_next(&mut self) -> Result<Option<DevInvalidation>, Self::Error>;
}

/// Build, publish and deploy once, then repeat for coalesced source changes.
/// Shutdown waits for an active deployment to finish. A failed watch pass is
/// reported and the next source change retries through the same product path.
#[cfg(target_os = "linux")]
pub async fn run<S, F, R>(
    mut source: Option<S>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    mut deploy: F,
) -> anyhow::Result<()>
where
    S: DevInvalidationSource,
    F: FnMut() -> R,
    R: Future<Output = anyhow::Result<()>>,
{
    loop {
        if *shutdown.borrow() {
            return Ok(());
        }
        let result = deploy().await;
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
            while let Some(event) = source.try_next()? {
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
