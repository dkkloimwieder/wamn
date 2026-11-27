//! Database connection resolution and readiness during environment provisioning.

use anyhow::Context as _;
use std::time::Duration;
use wamn_postgres::connection_readiness::connect_with_retry;
use wamn_postgres::database::{ConnectionConfig, connection_for};

use crate::registry::deployment;

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
) -> anyhow::Result<ConnectionConfig> {
    let instance = deployment::instance_suffix(registry, triple)
        .await?
        .with_context(|| format!("project deployment {triple} is absent"))?;
    let namespace = deployment::namespace(registry, &triple.org, &triple.env)
        .await?
        .with_context(|| format!("env {}/{} is not provisioned", triple.org, triple.env))?;
    Ok(connection_for(triple, &instance, &namespace))
}
