//! Database connection resolution and readiness during platform provisioning.

use anyhow::Context as _;
use std::time::Duration;
use wamn_registry::deployment::DeploymentStore as _;

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

/// Wait for the project database to accept an authenticated SQL connection.
///
/// # Errors
/// Names the database if it cannot be reached within 120 seconds.
pub async fn wait_project_connection(
    input: &str,
    database: &str,
) -> anyhow::Result<sqlx::PgConnection> {
    let timeout = Duration::from_secs(120);
    let mut last_error = None;
    let connection = connect_with_retry(input, |error| {
        last_error = Some(error);
        Ok(())
    });
    match tokio::time::timeout(timeout, connection).await {
        Ok(result) => result,
        Err(elapsed) => Err(last_error.unwrap_or_else(|| elapsed.into())),
    }
    .with_context(|| {
        format!(
            "project database {database} did not accept connections within {} seconds",
            timeout.as_secs()
        )
    })
}

/// Resolve the recorded database on its project's CNPG cluster using the caller's registry session.
pub async fn connection(
    registry: &mut sqlx::PgConnection,
    triple: &wamn_registry::Triple,
) -> anyhow::Result<crate::database::ConnectionConfig> {
    let instance = crate::Registry::new(registry)
        .instance_suffix(triple)
        .await?
        .with_context(|| format!("project deployment {triple} is absent"))?;
    Ok(crate::database::connection_for(triple, &instance))
}

/// Take the org lock for the lifetime of the dedicated connection.
pub async fn lock_org_login(
    connection: &mut sqlx::PgConnection,
    control: &wamn_role::LoginName,
) -> anyhow::Result<()> {
    sqlx::query("SELECT pg_advisory_lock(hashtextextended($1, 0))")
        .bind(control.as_str())
        .execute(connection)
        .await
        .context("lock the org login")?;
    Ok(())
}
