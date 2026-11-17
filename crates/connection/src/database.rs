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

/// Advisory slots shared by one application's calls across all hosts.
#[derive(Debug, Clone, Copy)]
pub struct CallLimit {
    key: i32,
    slots: i32,
}

impl CallLimit {
    pub fn new(connection: &uuid::Uuid, application: &str, slots: i32) -> anyhow::Result<Self> {
        use sha2::{Digest as _, Sha256};
        anyhow::ensure!(slots > 0, "call limit must be positive");
        let mut hash = Sha256::new();
        hash.update(connection.as_bytes());
        hash.update(application.as_bytes());
        let key = i32::from_be_bytes(
            hash.finalize()[..4]
                .try_into()
                .expect("SHA-256 has four bytes"),
        );
        Ok(Self { key, slots })
    }
}

/// The application exhausted its shared advisory slots until the call deadline.
#[derive(Debug)]
pub struct CallLimitReached;

impl std::fmt::Display for CallLimitReached {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("call limit reached")
    }
}
impl std::error::Error for CallLimitReached {}

/// A checked transaction release; dropping an unfinished guard closes its connection.
#[derive(Debug)]
pub struct ResourceLock {
    connection: ConnectionGuard,
    _local: Option<tokio::sync::OwnedMutexGuard<()>>,
}
impl ResourceLock {
    pub async fn acquire(
        reader: &crate::connection_credentials::ConnectionCredentials,
        project: &wamn_registry::ProjectId,
        environment: &wamn_registry::EnvName,
        locks: &ResourceLocks,
        exclusive: Option<&crate::connection_memory::ConnectionFact>,
        deadline: tokio::time::Instant,
        call_limit: Option<CallLimit>,
    ) -> anyhow::Result<Self> {
        use anyhow::Context as _;
        use sha2::{Digest as _, Sha256};
        use sqlx::Row as _;
        tokio::time::timeout_at(deadline, async {
            let (local, key) = if let Some(fact) = exclusive {
                let resource = fact
                    .resource_key
                    .as_deref()
                    .context("exclusive connection has no resource key")?;
                let local = locks.acquire(fact.target_id, resource, deadline).await?;
                let mut hash = Sha256::new();
                hash.update(fact.target_id.as_bytes());
                hash.update(resource.as_bytes());
                let key = i64::from_be_bytes(
                    hash.finalize()[..8]
                        .try_into()
                        .expect("SHA-256 has eight bytes"),
                );
                (Some(local), Some(key))
            } else {
                (None, None)
            };
            let mut lock = Self {
                connection: ConnectionGuard::new(
                    reader.oauth_database(project, environment).await?,
                    Arc::default(),
                ),
                _local: local,
            };
            let statement = match (call_limit, key) {
                (Some(CallLimit { key, slots }), Some(exclusive)) => format!(
                    "WITH slot AS MATERIALIZED (SELECT platform.call_slot({key}, {slots}) AS value) \
                     SELECT value, pg_advisory_xact_lock({exclusive}::bigint) FROM slot WHERE value IS NOT NULL"
                ),
                (Some(CallLimit { key, slots }), None) => {
                    format!("SELECT platform.call_slot({key}, {slots})")
                }
                (None, Some(key)) => format!("SELECT pg_advisory_xact_lock({key}::bigint)"),
                (None, None) => unreachable!("a call guard requires a limit or an exclusive lock"),
            };
            let mut begin = "BEGIN; ";
            let mut backoff = std::time::Duration::from_millis(10);
            loop {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() && call_limit.is_some() {
                    return Err(CallLimitReached.into());
                }
                anyhow::ensure!(!remaining.is_zero(), "connection lock deadline elapsed");
                let millis = remaining.as_millis().max(1);
                // Every interpolated value is a host-computed integer, never guest SQL.
                let rows = sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
                    "{begin}SET LOCAL lock_timeout = '{millis}ms'; {statement}"
                )))
                .fetch_all(lock.connection.connection())
                .await?;
                if call_limit.is_none()
                    || rows.first().is_some_and(|row| row.get::<Option<i32>, _>(0).is_some())
                {
                    return Ok(lock);
                }
                begin = "";
                tokio::time::timeout_at(deadline, tokio::time::sleep(backoff))
                    .await
                    .map_err(|_| CallLimitReached)?;
                backoff = (backoff * 2).min(std::time::Duration::from_millis(500));
            }
        })
        .await?
    }
    pub async fn release(mut self) -> anyhow::Result<()> {
        sqlx::raw_sql("ROLLBACK")
            .execute(self.connection.connection())
            .await?;
        self.connection.repool();
        Ok(())
    }
}
