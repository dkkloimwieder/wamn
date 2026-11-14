//! Registry migration history access.

/// SQLx's system history, owned by the registry schema.
pub const HISTORY_TABLE: &str = "registry._sqlx_migrations";

/// Read whether the system migration history exists.
///
/// # Errors
/// Returns database errors.
pub async fn history_exists(connection: &mut sqlx::PgConnection) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT to_regclass('registry._sqlx_migrations') IS NOT NULL")
        .fetch_one(connection)
        .await
}

/// Let the apply login inspect system migrations.
///
/// # Errors
/// Returns database errors.
pub async fn grant_history_read(connection: &mut sqlx::PgConnection) -> Result<(), sqlx::Error> {
    sqlx::raw_sql("GRANT SELECT ON registry._sqlx_migrations TO wamn_apply")
        .execute(connection)
        .await?;
    Ok(())
}
