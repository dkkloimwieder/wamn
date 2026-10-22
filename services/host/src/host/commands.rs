//! Workload identity policy around the runtime's default OCI start.

use std::sync::Arc;

use anyhow::Context as _;
use wash_runtime::washlet::{HostCommandHandler, HostControlDefaults, types::v2};

use wamn_execution_host::OperationHost;
use wamn_runtime::plugins::WamnPostgres;

pub(super) struct Commands {
    pub postgres: Arc<WamnPostgres>,
    pub operations: Option<Arc<OperationHost>>,
}

struct PendingStart<'a> {
    postgres: &'a WamnPostgres,
    workload: &'a str,
}

impl Drop for PendingStart<'_> {
    fn drop(&mut self) {
        self.postgres.forget_operation_start(self.workload);
    }
}

#[async_trait::async_trait]
impl HostCommandHandler for Commands {
    async fn start(
        &self,
        defaults: &HostControlDefaults,
        request: v2::WorkloadStartRequest,
    ) -> anyhow::Result<v2::WorkloadStartResponse> {
        let workload = request.workload.as_ref().context("start has no workload")?;
        let components = workload
            .wit_world
            .as_ref()
            .map(|world| &world.components[..])
            .unwrap_or_default();
        for component in components {
            wamn_catalog::validate_component_start_reference(&component.image)?;
        }
        let workload_id = request.workload_id.clone();
        let mut pending = None;
        if let Some(operations) = &self.operations
            && let Some(entry) = &operations.release.manifest().composed_entry
        {
            let mut entries = components
                .iter()
                .filter(|component| component.image == entry.reference);
            if let Some(component) = entries.next() {
                anyhow::ensure!(
                    entries.next().is_none(),
                    "workload repeats its composed entry"
                );
                anyhow::ensure!(
                    component.pool_size == 0,
                    "composed entry requires poolSize 0"
                );
                self.postgres.bind_operation_workload(
                    &workload_id,
                    Arc::clone(&operations.release),
                    &component.image,
                    &component.name,
                    operations.claims(
                        &operations.release.scope().tenant_id,
                        Some(operations.release_identity()),
                    ),
                )?;
                pending = Some(PendingStart {
                    postgres: &self.postgres,
                    workload: &workload_id,
                });
            }
        }
        // The runtime still owns reservations, pulls, verification, compilation and stop races.
        let result = defaults.start(request).await;
        drop(pending);
        result
    }
}
