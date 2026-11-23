//! Locks for environment lifecycle operations and application rollouts.
//! Application rollouts use distinct advisory keys, so other applications can proceed.

use anyhow::{Context as _, bail};
use sqlx::PgConnection as Client;
use sqlx::{Connection as _, Row as _};
use std::{sync::Arc, time::Duration};
use tokio::task::JoinHandle;

use wamn_registry::lifecycle_lock::{DEPLOYMENT_IN_PROGRESS, LifecycleLocks};
use wamn_registry::{ApplicationName, EnvName, OrgId, Triple};

/// Environment exclusion key shared by lifecycle operations and CDC publication.
/// Acquire this key before an application key or a database mutation lock.
pub const ENVIRONMENT_LOCK_SQL: &str =
    "SELECT pg_try_advisory_lock(hashtextextended('wamn-env/' || $1 || '/' || $2, 0))";

/// Transaction-scoped exclusion for integration mutations. Params: org, environment.
pub const ENVIRONMENT_TRANSACTION_LOCK_SQL: &str =
    "SELECT pg_try_advisory_xact_lock(hashtextextended('wamn-env/' || $1 || '/' || $2, 0))";

/// Independent applications and SQL Jobs exclude shared database replacement.
pub const ENVIRONMENT_SHARED_LOCK_SQL: &str =
    "SELECT pg_try_advisory_lock_shared(hashtextextended('wamn-env/' || $1 || '/' || $2, 0))";

/// A held lifecycle lock. A one-second heartbeat keeps the session alive.
/// PostgreSQL expires an idle session after five seconds, even behind a port forward.
pub struct LifecycleLock {
    client: Arc<tokio::sync::Mutex<Client>>,
    connection: JoinHandle<()>,
    lost: tokio::sync::watch::Receiver<bool>,
}

impl std::fmt::Debug for LifecycleLock {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LifecycleLock")
            .finish_non_exhaustive()
    }
}

impl LifecycleLocks for LifecycleLock {
    type Session = Client;

    /// Take the lock of `triple` on an opened connection.
    ///
    /// # Errors
    ///
    /// Returns a database error or refuses when another session holds the lock.
    async fn acquire(client: Client, triple: &Triple) -> anyhow::Result<Self> {
        Self::acquire_environment(client, &triple.org, &triple.env).await
    }

    /// Take exclusive ownership of the host shared by an org and environment.
    async fn acquire_environment(
        client: Client,
        org: &OrgId,
        env: &EnvName,
    ) -> anyhow::Result<Self> {
        let lock = Self::start(client).await?;
        if !lock
            .try_acquire(ENVIRONMENT_LOCK_SQL, &[org.as_str(), env.as_str()])
            .await?
        {
            bail!("{DEPLOYMENT_IN_PROGRESS}: environment {org}/{env}");
        }
        Ok(lock)
    }

    /// Wait for active deployments before taking exclusive environment ownership.
    async fn acquire_wait(client: Client, triple: &Triple) -> anyhow::Result<Self> {
        let lock = Self::start(client).await?;
        while !lock
            .try_acquire(
                ENVIRONMENT_LOCK_SQL,
                &[triple.org.as_str(), triple.env.as_str()],
            )
            .await?
        {}
        Ok(lock)
    }

    /// Serialize operations on one application without blocking another application's rollout.
    async fn acquire_application(
        client: Client,
        triple: &Triple,
        application: &ApplicationName,
    ) -> anyhow::Result<Self> {
        // Fixed order: environment exclusion, then the application key.
        let lock = Self::acquire_shared(client, triple).await?;
        let key = serde_json::to_string(&(&triple.org, &triple.project, &triple.env, application))?;
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
    async fn acquire_shared(client: Client, triple: &Triple) -> anyhow::Result<Self> {
        let lock = Self::start(client).await?;
        let held = lock
            .try_acquire(
                ENVIRONMENT_SHARED_LOCK_SQL,
                &[triple.org.as_str(), triple.env.as_str()],
            )
            .await?;
        if !held {
            bail!("{DEPLOYMENT_IN_PROGRESS}: environment {triple}");
        }
        Ok(lock)
    }
}

impl LifecycleLock {
    async fn start(mut client: Client) -> anyhow::Result<Self> {
        sqlx::raw_sql("SET idle_session_timeout = '5s'")
            .execute(&mut client)
            .await?;
        let client = Arc::new(tokio::sync::Mutex::new(client));
        let heartbeat = Arc::clone(&client);
        let (sender, lost) = tokio::sync::watch::channel(false);
        let connection = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let mut client = heartbeat.lock().await;
                if !matches!(
                    tokio::time::timeout(Duration::from_secs(3), client.ping()).await,
                    Ok(Ok(()))
                ) {
                    sender.send_replace(true);
                    return;
                }
            }
        });
        Ok(Self {
            client,
            connection,
            lost,
        })
    }

    async fn try_acquire(&self, sql: &str, params: &[&str]) -> anyhow::Result<bool> {
        // Allow a dead holder's five-second idle timeout to expire before refusing.
        tokio::time::timeout(Duration::from_secs(6), async {
            loop {
                let mut client = self.client.lock().await;
                let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()));
                for value in params {
                    query = query.bind(value);
                }
                if query.fetch_one(&mut *client).await?.get::<bool, _>(0) {
                    return Ok::<_, sqlx::Error>(true);
                }
                drop(client);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap_or(Ok(false))
        .context("take the lifecycle lock")
    }

    /// Release the lock before the verb returns, without waiting for session expiry.
    pub async fn release(self) {
        self.connection.abort();
        if *self.lost.borrow() {
            return;
        }
        let _ = sqlx::query("SELECT pg_advisory_unlock_all()")
            .execute(&mut *self.client.lock().await)
            .await
            .map(|result| result.rows_affected());
    }

    /// Run `verb` under the lock of `triple`, and release the lock when the
    /// verb ends, with its result or its error.
    ///
    /// # Errors
    ///
    /// When the lock is held, or `verb` fails.
    pub async fn hold<T>(
        client: Client,
        triple: &Triple,
        verb: impl AsyncFnOnce(&Self) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let lock = Self::acquire(client, triple).await?;
        let result = lock.protect(verb(&lock)).await;
        lock.release().await;
        result
    }

    /// Cancel protected work when the session that owns its advisory locks is lost.
    ///
    /// # Errors
    /// Returns the operation error, or an error when the lock session is lost.
    pub fn protect<'a, T: 'a>(
        &'a self,
        work: impl std::future::Future<Output = anyhow::Result<T>> + 'a,
    ) -> impl std::future::Future<Output = anyhow::Result<T>> + 'a {
        let work = Box::pin(work);
        let mut lost = self.lost.clone();
        async move {
            tokio::select! {
                biased;
                _ = lost.wait_for(|lost| *lost) => bail!("lifecycle lock lost; operation stopped"),
                result = work => result,
            }
        }
    }

    /// Borrow the session that owns the advisory locks for SQL, including
    /// transactions. The heartbeat waits for this guard. End any transaction
    /// and drop the guard before waiting on external work.
    pub async fn client(&self) -> tokio::sync::MutexGuard<'_, Client> {
        self.client.lock().await
    }
}

impl Drop for LifecycleLock {
    fn drop(&mut self) {
        self.connection.abort();
    }
}
