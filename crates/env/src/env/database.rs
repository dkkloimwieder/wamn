//! Database operations and guarded sessions used by deployment.

use chrono::{DateTime, Utc};
use wamn_registry::Triple;

use crate::env_sql_effects::{PostgresSession, lock_env};

#[derive(Debug)]
pub(crate) enum DatabaseConnection<'a> {
    Direct(&'a str),
    WaitForEnv { input: &'a str, triple: &'a Triple },
    Wait { input: &'a str, database: &'a str },
    Delete { input: &'a str, data: bool },
}

#[derive(Debug)]
pub struct PendingCaptureGap<'a> {
    pub triple: &'a Triple,
    pub name: &'a str,
    pub reason: &'a str,
    pub start_lsn: Option<&'a str>,
    pub start_at: DateTime<Utc>,
    pub end_lsn: &'a str,
}

#[derive(Debug)]
pub struct CaptureRegistration {
    pub name: String,
    pub stream: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug)]
pub struct CaptureStatus {
    pub reason: String,
    pub confirmed: Option<String>,
}

#[derive(Debug)]
pub struct ConnectionBinding {
    pub id: String,
    pub integration_type: String,
    pub parameters: serde_json::Value,
    pub generation: i64,
    pub target: String,
    pub authority: Option<String>,
}

#[derive(Debug)]
pub struct IntegrationBinding {
    pub integration_type: String,
    pub runtime: String,
    pub schema: Option<serde_json::Value>,
    pub digest: Option<String>,
    pub exclusive: bool,
    pub exports: Vec<String>,
    pub write_interfaces: Vec<String>,
    pub placement: String,
}

/// The database session and its loss signal. The owner releases it after protected work.
#[derive(Debug)]
pub(crate) struct LifecycleLock {
    pub(crate) session: PostgresSession,
    pub(crate) lost: tokio::sync::watch::Receiver<bool>,
}

impl LifecycleLock {
    pub(crate) async fn release(self) {
        self.session.release().await;
    }

    pub(crate) async fn hold<T>(
        connection: DatabaseConnection<'_>,
        triple: &Triple,
        operation: impl AsyncFnOnce(&PostgresSession) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let lock = lock_env(connection, &triple.org, &triple.env).await?;
        let result = lock.protect(operation(&lock.session)).await;
        lock.release().await;
        result
    }

    pub(crate) fn protect<T>(
        &self,
        operation: impl Future<Output = anyhow::Result<T>>,
    ) -> impl Future<Output = anyhow::Result<T>> {
        // Keep the guarded SQL futures out of the worker's stack frame.
        let operation = Box::pin(operation);
        async move {
            let mut lost = self.lost.clone();
            tokio::select! {
                biased;
                _ = lost.wait_for(|lost| *lost) => anyhow::bail!("lifecycle lock lost; operation stopped"),
                result = operation => result,
            }
        }
    }
}

/// Reason recorded when a capture position is absent.
pub const MISSING_CAPTURE_REASON: &str = "missing";
