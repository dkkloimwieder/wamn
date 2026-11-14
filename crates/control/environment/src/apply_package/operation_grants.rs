use anyhow::Context as _;
use wamn_control_provision::operation_grants::{
    ENSURE_ADMIN_ROLE_SQL, OPERATION_GRANT_LOCK_SQL, OPERATION_GRANT_TRANSACTION_PRELUDE_SQL,
    operation_base_grants_requirements_sql,
};

/// Create the project's built-in `admin` role when it is absent, and return
/// whether a row was written. `admin` has no permission rows.
pub(super) async fn ensure_admin_role(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    project_env_key: &str,
    drops: &mut wamn_broker::auth_changes::AuthDrops,
) -> anyhow::Result<bool> {
    sqlx::query(OPERATION_GRANT_LOCK_SQL)
        .bind(project_env_key)
        .fetch_one(&mut **tx)
        .await
        .context("lock the project operation-grant carrier")?;
    sqlx::raw_sql(OPERATION_GRANT_TRANSACTION_PRELUDE_SQL)
        .execute(&mut **tx)
        .await
        .map(drop)
        .context("disable row filtering for the admin role write")?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(operation_base_grants_requirements_sql()))
        .execute(&mut **tx)
        .await
        .map(drop)
        .context("verify the application authorization base grants")?;
    let written = sqlx::query(ENSURE_ADMIN_ROLE_SQL)
        .execute(&mut **tx)
        .await
        .map(|result| result.rows_affected())
        .context("create the admin role")?;
    if written > 0 {
        drops.project_env_key(project_env_key);
    }
    Ok(written > 0)
}
