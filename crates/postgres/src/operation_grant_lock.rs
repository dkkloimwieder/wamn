//! Shared transaction lock for project role and permission writes.

use wamn_registry::ProjectEnvKey;

/// Serialize role and permission writes for the project environment.
///
/// # Errors
/// Returns a database error if the transaction cannot acquire the lock.
pub async fn lock_operation_grants(
    connection: &mut sqlx::PgConnection,
    key: &ProjectEnvKey,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended(\
     'wamn.operation-grants:' || $1, 0))",
    )
    .bind(key.as_str())
    .fetch_one(connection)
    .await?;
    Ok(())
}
