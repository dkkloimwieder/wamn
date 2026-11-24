//! Authorized project reads over the existing executor connection pool.

pub use wamn_postgres::operation::ConnectionGuard;

/// The project pool opens a read transaction with the executor identity.
#[async_trait::async_trait]
pub trait ProjectDatabase: Send + Sync {
    /// Check out the platform pool for OAuth's own write transaction.
    async fn oauth_connection(
        &self,
        project: &wamn_registry::ProjectId,
    ) -> anyhow::Result<sqlx::pool::PoolConnection<sqlx::Postgres>>;

    async fn read_connection(
        &self,
        project: &wamn_registry::ProjectId,
    ) -> anyhow::Result<ConnectionGuard>;
}
