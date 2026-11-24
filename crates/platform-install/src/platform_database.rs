//! Database locks during platform provisioning.

use anyhow::Context as _;

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
