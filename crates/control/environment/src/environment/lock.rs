//! Locks for environment lifecycle operations, application rollouts, and short file writes.
//! Application rollouts use distinct advisory keys, so other applications can proceed.
//! The file lock covers only the read, merge, and atomic rename of the authored document.

use anyhow::{Context as _, bail};
use tokio::task::JoinHandle;
use tokio_postgres::{Client, NoTls};
use wamn_control_registry::Triple;

/// The refusal when another verb holds the lock.
pub const DEPLOYMENT_IN_PROGRESS: &str = "deployment in progress";

/// The lock: `pg_try_advisory_lock` on the coordinate's key.
const TRY_LOCK_SQL: &str =
    "SELECT pg_try_advisory_lock(hashtextextended('wamn-env/' || $1 || '/' || $2 || '/' || $3, 0))";

/// A held lifecycle lock. Dropping it closes the connection, which releases it.
pub struct LifecycleLock {
    client: Client,
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
        let (client, connection) = tokio_postgres::connect(system_url, NoTls)
            .await
            .context("connect to the system database for the lifecycle lock")?;
        let connection = tokio::spawn(async move {
            let _ = connection.await;
        });
        let env = triple.env.as_str();
        let held: bool = client
            .query_one(TRY_LOCK_SQL, &[&triple.org, &triple.project, &env])
            .await
            .context("take the lifecycle lock")?
            .get(0);
        if !held {
            connection.abort();
            bail!("{DEPLOYMENT_IN_PROGRESS}: another verb holds the lifecycle lock of {triple}");
        }
        Ok(Self { client, connection })
    }

    /// Serialize operations on one application without blocking another application's rollout.
    pub async fn acquire_application(
        system_url: &str,
        triple: &Triple,
        application: &str,
    ) -> anyhow::Result<Self> {
        let (client, connection) = tokio_postgres::connect(system_url, NoTls).await?;
        let connection = tokio::spawn(async move {
            let _ = connection.await;
        });
        let lock = Self { client, connection };
        let key = serde_json::to_string(&(
            &triple.org,
            &triple.project,
            triple.env.as_str(),
            application,
        ))?;
        let held: bool = lock
            .client
            .query_one(
                "SELECT pg_try_advisory_lock(hashtextextended('wamn-application/' || $1, 0))",
                &[&key],
            )
            .await?
            .get(0);
        if !held {
            bail!("{DEPLOYMENT_IN_PROGRESS}: application {application} in {triple}");
        }
        Ok(lock)
    }

    /// Release the lock before the verb returns. The server releases it
    /// anyway when the connection closes, but only once it reads the closed
    /// socket, and a port forward delays that.
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
