//! Authorized project reads over the existing executor connection pool.

use std::sync::{Arc, atomic::AtomicU64};
type Object = sqlx::pool::PoolConnection<sqlx::Postgres>;

/// The project pool opens a read transaction with the executor identity.
#[async_trait::async_trait]
pub trait ProjectDatabase: Send + Sync {
    /// Check out the platform pool for OAuth's own write transaction.
    async fn oauth_connection(
        &self,
        project: &wamn_registry::ProjectId,
    ) -> anyhow::Result<sqlx::pool::PoolConnection<sqlx::Postgres>>;

    async fn owned_connection(
        &self,
        project: &wamn_registry::ProjectId,
    ) -> anyhow::Result<sqlx::PgConnection>;

    async fn read_connection(
        &self,
        project: &wamn_registry::ProjectId,
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

/// Local serialization shared by all project readers of the host.
#[derive(Debug, Default)]
pub struct ResourceLocks(
    std::sync::Mutex<
        std::collections::HashMap<(uuid::Uuid, String), std::sync::Weak<tokio::sync::Mutex<()>>>,
    >,
);

impl ResourceLocks {
    pub async fn acquire(
        &self,
        target: uuid::Uuid,
        resource: &str,
        deadline: tokio::time::Instant,
    ) -> anyhow::Result<tokio::sync::OwnedMutexGuard<()>> {
        let mutex = {
            let mut locks = self.0.lock().expect("resource locks poisoned");
            locks.retain(|_, lock| lock.strong_count() != 0);
            let key = (target, resource.to_owned());
            match locks.get(&key).and_then(std::sync::Weak::upgrade) {
                Some(lock) => lock,
                None => {
                    let lock = Arc::new(tokio::sync::Mutex::new(()));
                    locks.insert(key, Arc::downgrade(&lock));
                    lock
                }
            }
        };
        Ok(tokio::time::timeout_at(deadline, mutex.lock_owned()).await?)
    }
}

#[derive(Debug)]
enum LockConnection {
    Pool(ConnectionGuard),
    Owned(sqlx::PgConnection),
}

/// A checked transaction release; dropping an unfinished lock closes its connection.
#[derive(Debug)]
pub struct ResourceLock {
    connection: LockConnection,
    _local: tokio::sync::OwnedMutexGuard<()>,
}
impl ResourceLock {
    fn connection(&mut self) -> &mut sqlx::PgConnection {
        match &mut self.connection {
            LockConnection::Pool(connection) => connection.connection(),
            LockConnection::Owned(connection) => connection,
        }
    }
    pub async fn acquire(
        reader: &crate::connection_credentials::ConnectionCredentials,
        project: &wamn_registry::ProjectId,
        environment: &wamn_registry::EnvName,
        locks: &ResourceLocks,
        fact: &crate::connection_memory::ConnectionFact,
        deadline: tokio::time::Instant,
        owned: bool,
    ) -> anyhow::Result<Self> {
        use anyhow::Context as _;
        use sha2::{Digest as _, Sha256};
        let resource = fact
            .resource_key
            .as_deref()
            .context("exclusive connection has no resource key")?;
        let local = locks.acquire(fact.target_id, resource, deadline).await?;
        tokio::time::timeout_at(deadline, async {
            let connection = if owned {
                LockConnection::Owned(reader.owned_database(project, environment).await?)
            } else {
                LockConnection::Pool(ConnectionGuard::new(
                    reader.oauth_database(project, environment).await?,
                    Arc::default(),
                ))
            };
            let mut lock = Self {
                connection,
                _local: local,
            };
            let mut hash = Sha256::new();
            hash.update(fact.target_id.as_bytes());
            hash.update(resource.as_bytes());
            let key = i64::from_be_bytes(
                hash.finalize()[..8]
                    .try_into()
                    .expect("SHA-256 has eight bytes"),
            );
            sqlx::raw_sql("BEGIN").execute(lock.connection()).await?;
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            anyhow::ensure!(!remaining.is_zero(), "connection lock deadline elapsed");
            let millis = if owned {
                wamn_engine::engine::MAX_HOST_CALL_DURATION.as_millis()
            } else {
                remaining.as_millis().max(1)
            };
            // Only a host-computed integer is interpolated; PostgreSQL SET takes no bind parameter.
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
                "SET LOCAL lock_timeout = '{millis}ms'"
            )))
            .execute(lock.connection())
            .await?;
            sqlx::query("SELECT pg_advisory_xact_lock($1)")
                .bind(key)
                .execute(lock.connection())
                .await?;
            Ok(lock)
        })
        .await?
    }
    pub async fn release(mut self) -> anyhow::Result<()> {
        sqlx::raw_sql("ROLLBACK").execute(self.connection()).await?;
        if let LockConnection::Pool(connection) = self.connection {
            connection.repool();
        }
        Ok(())
    }
}
