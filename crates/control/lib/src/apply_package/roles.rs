use anyhow::{Context as _, ensure};
use tokio_postgres::Transaction;
use wamn_control_provision::DB_OWNER_ROLE;

const SELECT_ROLE_CONTEXT_SQL: &str = "SELECT current_user::text, session_user::text";

pub(super) async fn set_package_owner_role(tx: &Transaction<'_>) -> anyhow::Result<()> {
    tx.batch_execute(&format!("SET LOCAL ROLE \"{DB_OWNER_ROLE}\""))
        .await
        .context("narrow package migration authority to wamn_db_owner")
}

pub(super) async fn reset_host_role(tx: &Transaction<'_>) -> anyhow::Result<()> {
    tx.batch_execute("RESET ROLE")
        .await
        .context("reset package migration authority before trusted writes")?;
    assert_host_role(tx).await
}

pub(super) async fn assert_host_role(tx: &Transaction<'_>) -> anyhow::Result<()> {
    let row = tx
        .query_one(SELECT_ROLE_CONTEXT_SQL, &[])
        .await
        .context("read server role context before trusted package writes")?;
    let current_role = row.get::<_, String>(0);
    let session_role = row.get::<_, String>(1);
    ensure!(
        current_role == session_role,
        "package-role-reset-refused: current role {current_role:?} differs from session role {session_role:?}"
    );
    Ok(())
}
