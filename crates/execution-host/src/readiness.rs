//! Prepare one immutable release without changing host availability.

use std::collections::BTreeSet;
use std::time::Instant;

use crate::operation::OperationHost;

/// Verify bindings and initialize this release's components without running handlers.
pub async fn prepare_release(operations: &OperationHost) -> anyhow::Result<()> {
    let started = Instant::now();
    let components = operations.release.components();
    let digests: Vec<_> = components
        .iter()
        .map(|component| component.component_digest.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let bound = operations
        .context
        .connections
        .release_component_bindings_ready(
            operations.release.release().manifest_digest.as_str(),
            &digests,
        )
        .await?;
    anyhow::ensure!(bound, "release-component-requirement-unbound");
    operations.prepare_released(components).await?;
    tracing::info!(
        manifest_digest = %operations.release.release().manifest_digest,
        elapsed_ms = %started.elapsed().as_millis(),
        "release preparation completed"
    );
    Ok(())
}
