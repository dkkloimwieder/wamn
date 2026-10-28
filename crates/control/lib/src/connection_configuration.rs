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
    // Capture disablement destinations before the enablement row disappears.
    let environments: Vec<(String, String)> = tx
        .query(
            wamn_control_registry::sql::select_connection_projection_environments_sql(),
            &[&org, &operation, &payload],
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
            let drops = wamn_platform_identity::connections::project(
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

/// Import credentials into named connections for the named org.
pub async fn import(database_url: &str, org: &str, contents: &str) -> anyhow::Result<()> {
    let mut client = crate::environment::analyse::connect(database_url).await?;
    let tx = client.transaction().await?;
    use_role(&tx, WorkloadRoleFamily::Control).await?;
    wamn_platform_identity::connections::import_credentials(&tx, org, contents).await?;
    tx.commit().await?;
    Ok(())
}

/// Seed authority from retained environment bindings, then import credentials atomically.
/// The directory contains administration URLs named by `control_administration_key`.
pub async fn import_with_bindings(
    database_url: &str,
    org: &str,
    contents: &str,
    administration: &std::path::Path,
) -> anyhow::Result<()> {
    let mut system = crate::environment::analyse::connect(database_url).await?;
    let tx = system.transaction().await?;
    use_role(&tx, WorkloadRoleFamily::Control).await?;
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await?;
    for (project, env) in wamn_platform_identity::org::org_environments(&tx, org, None).await? {
        let url = std::fs::read_to_string(administration.join(
            wamn_control_provision::control_administration_key(&project, &env),
        ))?;
        let mut environment = connect_environment(url.trim()).await?;
        let environment_tx = environment.transaction().await?;
        use_role(&environment_tx, WorkloadRoleFamily::Administration).await?;
        let env_key = wamn_control_provision::project_env_key(org, &project, &env);
        let rows = environment_tx.query(
            "SELECT connection_name, integration FROM catalog.connection_upgrade_bindings($1,$2)",
            &[&env_key, &env],
        ).await.with_context(|| format!("read retained bindings in {project}/{env}"))?;
        for row in rows {
            let name: String = row.get(0);
            let integration: String = row.get(1);
            let source = tx
                .query_opt(
                    "SELECT integration FROM registry.connection_rows($1) WHERE name=$2",
                    &[&org, &name],
                )
                .await?
                .with_context(|| format!("unknown upgrade connection {name} in {project}/{env}"))?;
            anyhow::ensure!(
                source.get::<_, String>(0) == integration,
                "upgrade connection {name} has a different integration in {project}/{env}"
            );
            wamn_platform_identity::connections::enable_connection(&tx, org, &project, &name)
                .await?;
            for (scope, id) in [
                ("org", org.to_owned()),
                ("project", project.clone()),
                ("environment", format!("{project}/{env}")),
            ] {
                wamn_platform_identity::connections::allow_integration(
                    &tx,
                    org,
                    scope,
                    &id,
                    &integration,
                )
                .await?;
            }
        }
        environment_tx.commit().await?;
    }
    wamn_platform_identity::connections::import_credentials(&tx, org, contents).await?;
    tx.commit().await?;
    Ok(())
}

/// Rotate one connection credential without rewriting an older generation.
pub async fn rotate(
    database_url: &str,
    org: &str,
    name: &str,
    contents: &str,
) -> anyhow::Result<i64> {
    let mut client = crate::environment::analyse::connect(database_url).await?;
    let tx = client.transaction().await?;
    use_role(&tx, WorkloadRoleFamily::Control).await?;
    let generation = wamn_platform_identity::connections::rotate(&tx, org, name, contents).await?;
    tx.commit().await?;
    Ok(generation)
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
