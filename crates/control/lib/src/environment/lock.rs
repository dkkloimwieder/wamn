//! The lifecycle lock of one environment (docs/plan/platform-deploy.md R19).
//!
//! `apply`, `rollback` and `delete` take one session-level advisory lock in the
//! system database, keyed by the coordinate, on a connection of their own, and
//! hold it for the whole verb. A second verb on the same coordinate refuses at
//! once. A verb that ends releases the lock before it returns, so a next verb
//! in the same process finds it free. A killed verb releases the lock with its
//! connection, when the server reads the closed socket. No transaction is
//! open on the connection, so no row lock is held across the Helm call.
//! `--dry-run` and `show` take no lock.

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
