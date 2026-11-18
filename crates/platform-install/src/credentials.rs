//! Native CNPG credentials and public connection configuration.
use anyhow::Context as _;
use sqlx::Connection as _;
use std::time::Duration;
use wamn_postgres::{ConnectionConfig, SecretRef};
pub fn configuration(
    namespace: &str,
    cluster: &str,
    host: String,
    port: u16,
    database: String,
    username: wamn_identity_protocol::LoginName,
    secret: String,
) -> ConnectionConfig {
    ConnectionConfig {
        host,
        port,
        database,
        username,
        credentials: SecretRef {
            namespace: namespace.into(),
            name: secret,
        },
        server_ca: SecretRef {
            namespace: namespace.into(),
            name: format!("{cluster}-ca"),
        },
    }
}

pub async fn wait_role(
    connection: &mut sqlx::PgConnection,
    username: &wamn_identity_protocol::LoginName,
) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    loop {
        let present: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT FROM pg_roles WHERE rolname=$1 AND rolcanlogin)",
        )
        .bind(username)
        .fetch_one(&mut *connection)
        .await?;
        if present {
            return Ok(());
        }
        anyhow::ensure!(
            tokio::time::Instant::now() < deadline,
            "CNPG did not reconcile the database login"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Retry opening a connection while the caller accepts connection failures.
pub(crate) async fn connect_with_retry(
    input: &str,
    mut on_error: impl FnMut(anyhow::Error) -> anyhow::Result<()>,
) -> anyhow::Result<sqlx::PgConnection> {
    loop {
        match wamn_postgres::connect(input).await {
            Ok(connection) => return Ok(connection),
            Err(error) => on_error(error)?,
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Serialize org chart writes and cancel Helm if the registry session is lost.
pub(crate) async fn with_chart_lock<T>(
    registry_input: &str,
    namespace: &wamn_registry::OrgId,
    work: impl std::future::Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    let mut connection = wamn_postgres::connect(registry_input).await?;
    sqlx::query("SELECT pg_advisory_lock(hashtextextended('wamn.org-chart/' || $1, 0))")
        .bind(namespace)
        .execute(&mut connection)
        .await?;
    tokio::pin!(work);
    // The existing lifecycle operations use a one-second heartbeat as well.
    let mut heartbeat = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            result = &mut work => { connection.ping().await.context("org chart lock session was lost")?; return result; },
            _ = heartbeat.tick() => connection.ping().await.context("org chart lock session was lost")?,
        }
    }
}
