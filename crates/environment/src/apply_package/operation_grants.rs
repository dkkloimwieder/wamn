use anyhow::Context as _;
use wamn_postgres::operation::lock_operation_grants;
use wamn_provision::operation_grants::{
    ENSURE_ADMIN_ROLE_SQL, OPERATION_GRANT_TRANSACTION_PRELUDE_SQL,
    operation_base_grants_requirements_sql,
};

/// Create the project's built-in `admin` role when it is absent, and return
/// whether a row was written. `admin` has no permission rows.
pub(super) async fn ensure_admin_role(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    project_env_key: &wamn_registry::ProjectEnvKey,
) -> anyhow::Result<bool> {
    lock_operation_grants(tx, project_env_key)
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
    Ok(written > 0)
}
