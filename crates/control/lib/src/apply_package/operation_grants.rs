use anyhow::Context as _;
use tokio_postgres::Transaction;
use wamn_control_provision::operation_grants::{
    OPERATION_GRANT_LOCK_SQL, OPERATION_GRANT_TRANSACTION_PRELUDE_SQL,
    OperationGrantReconcileResult, operation_grant_floor_check_sql, reconcile_operation_grants_sql,
};

pub(super) async fn reconcile_package_operation_grants(
    tx: &Transaction<'_>,
    manifest_bytes: &[u8],
    tenant: &str,
) -> anyhow::Result<OperationGrantReconcileResult> {
    tx.query_one(OPERATION_GRANT_LOCK_SQL, &[&tenant])
        .await
        .context("lock the tenant operation-grant carrier")?;
    tx.batch_execute(OPERATION_GRANT_TRANSACTION_PRELUDE_SQL)
        .await
        .context("disable row filtering for package operation-grant reconciliation")?;
    tx.batch_execute(&operation_grant_floor_check_sql())
        .await
        .context("verify the application authorization floor")?;
    let statement = reconcile_operation_grants_sql(manifest_bytes, tenant)
        .context("derive exact package operation grants")?;
    let row = tx
        .query_one(&statement, &[])
        .await
        .context("reconcile exact package operation grants")?;
    Ok(OperationGrantReconcileResult::new(
        row.get("role_rows_changed"),
        row.get("grants_added"),
        row.get("grants_removed"),
    ))
}
