//! Native CNPG credentials and public connection configuration.
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
