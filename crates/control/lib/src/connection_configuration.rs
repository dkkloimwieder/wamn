//! Connection administration shared by the command line and control routes.

use wamn_control_provision::WorkloadRoleFamily;

/// Switch to the restricted ACL role whenever the session may, so a privileged
/// CLI login writes only with that role's grants. A workload login that inherits
/// the role without SET (the host) keeps its own session role.
pub(crate) async fn use_role(
    tx: &tokio_postgres::Transaction<'_>,
    family: WorkloadRoleFamily,
) -> anyhow::Result<()> {
    let role = family.acl_role();
    let may_set: bool = tx
        .query_one(
            "SELECT CASE WHEN EXISTS (SELECT 1 FROM pg_roles WHERE rolname = $1) \
             THEN pg_has_role(current_user, $1::text::name, 'SET') ELSE false END",
            &[&role],
        )
        .await?
        .get(0);
    if may_set {
        tx.batch_execute(&format!("SET LOCAL ROLE {role}")).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn use_role_without_the_role_keeps_the_session_role() {
        let mut server = wamn_test_postgres::start(&[]).unwrap();
        let database = server.create_database("use_role").unwrap();
        let (mut client, connection) =
            tokio_postgres::connect(database.url(), tokio_postgres::NoTls)
                .await
                .unwrap();
        let task = tokio::spawn(connection);
        let tx = client.transaction().await.unwrap();
        use_role(&tx, WorkloadRoleFamily::Control).await.unwrap();
        let role: String = tx
            .query_one("SELECT current_user::text", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(role, "postgres");
        drop(tx);
        drop(client);
        task.await.unwrap().unwrap();
    }
}
