use anyhow::Context as _;
use tokio_postgres::Transaction;
use wamn_control_provision::operation_grants::{
    ENSURE_ADMIN_ROLE_SQL, OPERATION_GRANT_LOCK_SQL, OPERATION_GRANT_TRANSACTION_PRELUDE_SQL,
    operation_grant_floor_check_sql,
};

/// Create the tenant's built-in `admin` role when it is absent, and return
/// whether a row was written. `admin` has no permission rows.
pub(super) async fn ensure_admin_role(tx: &Transaction<'_>, tenant: &str) -> anyhow::Result<bool> {
    tx.query_one(OPERATION_GRANT_LOCK_SQL, &[&tenant])
        .await
        .context("lock the tenant operation-grant carrier")?;
    tx.batch_execute(OPERATION_GRANT_TRANSACTION_PRELUDE_SQL)
        .await
        .context("disable row filtering for the admin role write")?;
    tx.batch_execute(&operation_grant_floor_check_sql())
        .await
        .context("verify the application authorization floor")?;
    let written = tx
        .execute(ENSURE_ADMIN_ROLE_SQL, &[&tenant])
        .await
        .context("create the admin role")?;
    Ok(written > 0)
}
