//! Database locks during platform provisioning.

use anyhow::Context as _;

/// Take the org lock for the lifetime of the dedicated connection.
pub async fn lock_org_login(
    connection: &mut sqlx::PgConnection,
    control: &wamn_role::LoginName,
) -> anyhow::Result<()> {
    lock_login(connection, control.as_str())
        .await
        .context("lock the org login")
}

/// Hold the worker login lock across SQL commit and Secret publication.
pub async fn lock_env_login(
    connection: &mut sqlx::PgConnection,
    org: &wamn_registry::OrgId,
    env: &wamn_registry::EnvName,
) -> anyhow::Result<()> {
    let login: String =
        sqlx::query_scalar("SELECT registry.env_worker_login($1, $2, current_database())")
            .bind(org.as_str())
            .bind(env.as_str())
            .fetch_one(&mut *connection)
            .await
            .context("resolve the env worker login")?;
    lock_login(connection, &login)
        .await
        .context("lock the env worker login")
}

async fn lock_login(connection: &mut sqlx::PgConnection, login: &str) -> anyhow::Result<()> {
    sqlx::query("SELECT pg_advisory_lock(hashtextextended($1, 0))")
        .bind(login)
        .execute(connection)
        .await?;
    Ok(())
}
