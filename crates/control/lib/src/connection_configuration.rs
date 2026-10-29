//! Connection administration shared by the command line and control routes.

use anyhow::Context as _;
use serde_json::Value;
use wamn_control_provision::WorkloadRoleFamily;
use wamn_platform_identity::auth_changes::{AuthChanges, AuthDrops};

/// Write one org configuration operation as the control role.
pub async fn write(
    database_url: &str,
    org: &str,
    operation: &str,
    payload: &str,
    changes: &AuthChanges,
    administration: Option<&std::path::Path>,
) -> anyhow::Result<Value> {
    let mut client = crate::environment::analyse::connect(database_url).await?;
    let tx = client.transaction().await?;
    use_role(&tx, WorkloadRoleFamily::Control).await?;
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await?;
    tx.query_one(
        "SELECT pg_advisory_xact_lock(hashtextextended('connections:' || $1, 0))",
        &[&org],
    )
    .await?;
    let request: Value = serde_json::from_str(payload)?;
    let selection =
        serde_json::json!({"project":request["project"],"env":request["env"]}).to_string();
    let environments: Vec<(String, String)> = tx
        .query(
            wamn_control_registry::sql::select_connection_projection_environments_sql(),
            &[&org, &operation, &selection],
        )
        .await?
        .into_iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    if !environments.is_empty() {
        administration
            .context("connection projections require the administration credential directory")?;
    }
    let mut drops = AuthDrops::default();
    let result =
        wamn_platform_identity::connections::write(&tx, org, operation, payload, &mut drops)
            .await
            .context("write connection configuration")?;
    tx.commit().await?;
    changes.publish(drops).await;
    let source = client.transaction().await?;
    use_role(&source, WorkloadRoleFamily::Control).await?;
    for (project, env) in environments {
        let projection = async {
            let url = std::fs::read_to_string(administration.expect("administration directory").join(
                wamn_control_provision::control_administration_key(&project, &env),
            ))?;
            let mut environment = connect_environment(url.trim()).await?;
            let environment_tx = environment.transaction().await?;
            use_role(&environment_tx, WorkloadRoleFamily::Administration).await?;
            environment_tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
                wamn_control_provision::PlatformComponent::Provisioning,
            )).await?;
            let drops = wamn_platform_identity::connection_projection::project(
                &source, &environment_tx, org, &project, &env,
            ).await?;
            environment_tx.commit().await?;
            Ok::<_, anyhow::Error>(drops)
        }.await.with_context(|| format!("connection authority committed; projection incomplete in {project}/{env}; reapply the environment"))?;
        changes.publish(projection).await;
    }
    source.commit().await?;
    Ok(result)
}

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

async fn connect_environment(url: &str) -> anyhow::Result<tokio_postgres::Client> {
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        crate::environment::analyse::connect(url),
    )
    .await
    .context("environment connection timed out")?
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
