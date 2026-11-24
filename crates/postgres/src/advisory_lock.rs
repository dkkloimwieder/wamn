//! Session advisory locks shared by database clients.

use sqlx::PgConnection;
use wamn_registry::{EnvName, OrgId};

/// Try to take a session lock that excludes concurrent environment deployment.
///
/// # Errors
/// Returns an error if the database cannot attempt the lock.
pub async fn try_environment_lock(
    connection: &mut PgConnection,
    org: &OrgId,
    env: &EnvName,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(crate::ENVIRONMENT_LOCK_SQL)
        .bind(org.as_str())
        .bind(env.as_str())
        .fetch_one(connection)
        .await
}

/// Release all advisory locks on the caller's database session.
///
/// # Errors
/// Returns an error if the database cannot release the locks.
pub async fn unlock_all_advisory_locks(connection: &mut PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_unlock_all()")
        .fetch_one(connection)
        .await?;
    Ok(())
}
