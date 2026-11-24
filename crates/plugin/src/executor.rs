//! Native executor access and the connection facts adapter.

use std::sync::Arc;

use wamn_postgres::WamnPostgres;
use wamn_postgres::operation::{ConnectionGuard, open_executor_read};
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

/// Connection facts use the same executor read without retaining the operation plugin.
#[derive(Debug)]
pub struct OperationDatabase {
    pools: Arc<WamnPostgres>,
}

impl OperationDatabase {
    /// Use the same project pools as the operation host.
    pub fn new(pools: Arc<WamnPostgres>) -> Self {
        Self { pools }
    }
}

#[async_trait::async_trait]
impl wamn_connection::database::ProjectDatabase for OperationDatabase {
    async fn oauth_connection(
        &self,
        project: &wamn_registry::ProjectId,
    ) -> anyhow::Result<sqlx::pool::PoolConnection<sqlx::Postgres>> {
        wamn_authentication::authorization::oauth_connection(&self.pools, project).await
    }

    async fn read_connection(
        &self,
        project: &wamn_registry::ProjectId,
    ) -> anyhow::Result<ConnectionGuard> {
        Ok(open_executor_read(&self.pools, project).await?)
    }
}
