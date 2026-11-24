//! Connection retries and database role readiness.

use std::time::Duration;

pub async fn wait_role(
    connection: &mut sqlx::PgConnection,
    username: &wamn_role::LoginName,
) -> anyhow::Result<()> {
    wait_role_login(connection, username, true).await
}

pub async fn wait_group_role(
    connection: &mut sqlx::PgConnection,
    role: &wamn_role::LoginName,
) -> anyhow::Result<()> {
    wait_role_login(connection, role, false).await
}

async fn wait_role_login(
    connection: &mut sqlx::PgConnection,
    role: &wamn_role::LoginName,
    can_login: bool,
) -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let present: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT FROM pg_roles WHERE rolname=$1 AND rolcanlogin=$2)",
            )
            .bind(role.as_str())
            .bind(can_login)
            .fetch_one(&mut *connection)
            .await?;
            if present {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("CNPG did not reconcile database role {role}"))?
}

/// Retry opening a connection while the caller accepts connection failures.
pub async fn connect_with_retry(
    input: &str,
    mut on_error: impl FnMut(anyhow::Error) -> anyhow::Result<()>,
) -> anyhow::Result<sqlx::PgConnection> {
    loop {
        match crate::connect(input).await {
            Ok(connection) => return Ok(connection),
            Err(error) => on_error(error)?,
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
