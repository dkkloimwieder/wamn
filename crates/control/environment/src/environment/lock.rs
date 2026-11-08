//! Locks for environment lifecycle operations, application rollouts, and short file writes.
//! Application rollouts use distinct advisory keys, so other applications can proceed.
//! The file lock covers only the read, merge, and atomic rename of the authored document.

use anyhow::{Context as _, bail};
use std::{sync::Arc, time::Duration};
use tokio::task::JoinHandle;
use tokio_postgres::{Client, NoTls, types::ToSql};
use wamn_control_registry::Triple;

/// The refusal when another verb holds the lock.
pub const DEPLOYMENT_IN_PROGRESS: &str = "deployment in progress";

use wamn_control_registry::sql::{
    ENVIRONMENT_LOCK_SQL as TRY_LOCK_SQL, ENVIRONMENT_SHARED_LOCK_SQL as TRY_SHARED_LOCK_SQL,
};

/// A held lifecycle lock. A one-second heartbeat keeps the session alive.
/// PostgreSQL expires an idle session after five seconds, even behind a port forward.
pub struct LifecycleLock {
    client: Arc<Client>,
    connection: JoinHandle<()>,
}

impl std::fmt::Debug for LifecycleLock {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LifecycleLock")
            .finish_non_exhaustive()
    }
}

impl LifecycleLock {
    /// Take the lock of `triple`, or refuse with "deployment in progress".
    ///
    /// # Errors
    ///
    /// When the system database refuses the connection, or another session
    /// holds the lock.
    pub async fn acquire(system_url: &str, triple: &Triple) -> anyhow::Result<Self> {
        let lock = Self::connect(system_url).await?;
        let env = triple.env.as_str();
        let held = lock
            .try_acquire(TRY_LOCK_SQL, &[&triple.org, &triple.project, &env])
            .await?;
        if !held {
            bail!("{DEPLOYMENT_IN_PROGRESS}: another verb holds the lifecycle lock of {triple}");
        }
        Ok(lock)
    }

    /// Wait for active deployments before taking exclusive environment ownership.
    pub async fn acquire_wait(system_url: &str, triple: &Triple) -> anyhow::Result<Self> {
        let lock = Self::connect(system_url).await?;
        while !lock
            .try_acquire(
                TRY_LOCK_SQL,
                &[&triple.org, &triple.project, &triple.env.as_str()],
            )
            .await?
        {}
        Ok(lock)
    }

    /// Serialize operations on one application without blocking another application's rollout.
    pub async fn acquire_application(
        system_url: &str,
        triple: &Triple,
        application: &str,
    ) -> anyhow::Result<Self> {
        // Fixed order: environment exclusion, then the application key.
        let lock = Self::acquire_shared(system_url, triple).await?;
        let key = serde_json::to_string(&(
            &triple.org,
            &triple.project,
            triple.env.as_str(),
            application,
        ))?;
        let held = lock
            .try_acquire(
                "SELECT pg_try_advisory_lock(hashtextextended('wamn-application/' || $1, 0))",
                &[&key],
            )
            .await?;
        if !held {
            bail!("{DEPLOYMENT_IN_PROGRESS}: application {application} in {triple}");
        }
        Ok(lock)
    }

    /// Exclude database replacement while allowing independent application operations.
    /// SQL Jobs hold this guard in their own process, even after their caller exits.
    pub async fn acquire_shared(system_url: &str, triple: &Triple) -> anyhow::Result<Self> {
        let lock = Self::connect(system_url).await?;
        let held = lock
            .try_acquire(
                TRY_SHARED_LOCK_SQL,
                &[&triple.org, &triple.project, &triple.env.as_str()],
            )
            .await?;
        if !held {
            bail!("{DEPLOYMENT_IN_PROGRESS}: environment {triple}");
        }
        Ok(lock)
    }

    async fn connect(system_url: &str) -> anyhow::Result<Self> {
        let (client, connection) = tokio_postgres::connect(system_url, NoTls)
            .await
            .context("connect to the system database for the lifecycle lock")?;
        let client = Arc::new(client);
        let heartbeat = Arc::clone(&client);
        let connection = tokio::spawn(async move {
            tokio::select! {
                _ = connection => {},
                () = async {
                    loop {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        if heartbeat.simple_query("SELECT 1").await.is_err() {
                            break;
                        }
                    }
                } => {},
            }
        });
        let lock = Self { client, connection };
        lock.client
            .batch_execute("SET idle_session_timeout = '5s'")
            .await
            .context("set the lifecycle lock session timeout")?;
        Ok(lock)
    }

    async fn try_acquire(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> anyhow::Result<bool> {
        // Allow a dead holder's five-second idle timeout to expire before refusing.
        tokio::time::timeout(Duration::from_secs(6), async {
            loop {
                if self.client.query_one(sql, params).await?.get::<_, bool>(0) {
                    return Ok::<_, tokio_postgres::Error>(true);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap_or(Ok(false))
        .context("take the lifecycle lock")
    }

    /// Release the lock before the verb returns, without waiting for session expiry.
    pub async fn release(self) {
        let _ = self
            .client
            .execute("SELECT pg_advisory_unlock_all()", &[])
            .await;
    }

    /// Run `verb` under the lock of `triple`, and release the lock when the
    /// verb ends, with its result or its error.
    ///
    /// # Errors
    ///
    /// When the lock is held, or `verb` fails.
    pub async fn hold<T>(
        system_url: &str,
        triple: &Triple,
        verb: impl AsyncFnOnce(&Self) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let lock = Self::acquire(system_url, triple).await?;
        let result = verb(&lock).await;
        lock.release().await;
        result
    }

    /// The connection that holds the lock. A phase may read on it, and must
    /// leave no transaction open.
    pub fn client(&self) -> &Client {
        &self.client
    }
}

impl Drop for LifecycleLock {
    fn drop(&mut self) {
        self.connection.abort();
    }
}

/// Hold only the read/merge/rename of an environment document.
pub(super) fn document_lock(file: &std::path::Path) -> anyhow::Result<std::fs::File> {
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(file.with_extension("k.lock"))?;
    lock.lock().context("lock the environment document")?;
    Ok(lock)
}
