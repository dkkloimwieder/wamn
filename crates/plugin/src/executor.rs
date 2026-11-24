//! Native executor access and the connection facts adapter.

use wamn_run_state::run_store::ProductionLeaseError;

use super::OperationHost;

impl OperationHost {
    pub async fn executor_read<T>(
        &self,
        project: &wamn_registry::ProjectId,
        read: impl AsyncFnOnce(&mut sqlx::PgConnection) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        self.pools.executor_read(project, read).await
    }

    pub async fn executor_write<T, E>(
        &self,
        project: &wamn_registry::ProjectId,
        write: impl AsyncFnOnce(&mut sqlx::PgConnection) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<ProductionLeaseError>,
    {
        self.pools.executor_write(project, write).await
    }
}
