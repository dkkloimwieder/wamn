use std::sync::{Arc, atomic::AtomicU64};

use crate::Object;

/// Owns an acquired connection across an await.
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
            crate::destroy_connection(connection, &self.destroyed);
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
