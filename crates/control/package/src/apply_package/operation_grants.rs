use anyhow::Context as _;
use tokio_postgres::Transaction;
use wamn_control_provision::operation_grants::{
    ENSURE_ADMIN_ROLE_SQL, OPERATION_GRANT_LOCK_SQL, OPERATION_GRANT_TRANSACTION_PRELUDE_SQL,
    operation_base_grants_check_sql,
};

/// Create the project's built-in `admin` role when it is absent, and return
/// whether a row was written. `admin` has no permission rows.
pub(super) async fn ensure_admin_role(
    tx: &Transaction<'_>,
    project_env_key: &str,
    drops: &mut wamn_broker::auth_changes::AuthDrops,
) -> anyhow::Result<bool> {
    tx.query_one(OPERATION_GRANT_LOCK_SQL, &[&project_env_key])
        .await
        .context("lock the project operation-grant carrier")?;
    tx.batch_execute(OPERATION_GRANT_TRANSACTION_PRELUDE_SQL)
        .await
        .context("disable row filtering for the admin role write")?;
    tx.batch_execute(&operation_base_grants_check_sql())
        .await
        .context("verify the application authorization base grants")?;
    let written = tx
        .execute(ENSURE_ADMIN_ROLE_SQL, &[])
        .await
        .context("create the admin role")?;
    if written > 0 {
        drops.project_env_key(project_env_key);
    }
    Ok(written > 0)
}
