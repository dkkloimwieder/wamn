//! Authorized project reads over the existing executor connection pool.

use std::sync::{Arc, atomic::AtomicU64};
type Object = sqlx::pool::PoolConnection<sqlx::Postgres>;

/// The project pool opens a read transaction with the executor identity.
#[async_trait::async_trait]
pub trait ProjectDatabase: Send + Sync {
    async fn read_connection(
        &self,
        project: &str,
        project_env_key: &str,
    ) -> anyhow::Result<ConnectionGuard>;
}

/// Owns a acquired connection across an await.
///
/// Dropping an armed guard destroys the connection, so cancellation cannot
/// fast-repool a session whose identity transaction may still be open.
#[derive(Debug)]
pub struct ConnectionGuard {
    connection: Option<Object>,
    destroyed: Arc<AtomicU64>,
}

impl ConnectionGuard {
    pub fn new(connection: Object, destroyed: Arc<AtomicU64>) -> Self {
        Self {
            connection: Some(connection),
            destroyed,
        }
    }

    pub fn connection(&mut self) -> &mut sqlx::PgConnection {
        self.connection
            .as_mut()
            .expect("armed statement connection guard has a connection")
    }

    pub fn into_connection(mut self) -> Object {
        self.connection
            .take()
            .expect("armed statement connection guard has a connection")
    }

    pub fn repool(mut self) {
        drop(self.connection.take());
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            self.destroyed
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            drop(connection.detach());
        }
    }
}

impl ConnectionGuard {
    /// End an authorized read and return its clean connection to the existing pool.
    pub async fn commit(mut self) -> anyhow::Result<()> {
        sqlx::raw_sql("COMMIT").execute(self.connection()).await?;
        self.repool();
        Ok(())
    }
}
