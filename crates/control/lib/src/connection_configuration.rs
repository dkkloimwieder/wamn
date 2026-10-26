//! Connection administration shared by the command line and control routes.

use anyhow::Context as _;
use serde_json::Value;
use wamn_platform_identity::auth_changes::{AuthChanges, AuthDrops};

/// Write one org configuration operation as the system owner, without superuser.
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
    tx.batch_execute("SET LOCAL ROLE wamn_system").await?;
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await?;
    let mut drops = AuthDrops::default();
    let mut targets = Vec::new();
    for (project, env) in wamn_platform_identity::org::org_environments(&tx, org, None).await? {
        let directory = administration
            .context("connection projections require the administration credential directory")?;
        let url = std::fs::read_to_string(directory.join(
            wamn_control_provision::control_administration_key(&project, &env),
        ))?;
        let target = crate::environment::analyse::connect(url.trim()).await?;
        wamn_platform_identity::connections::check_applied_rename(
            &tx, &target, org, operation, payload,
        )
        .await?;
        targets.push((project, env, target));
    }
    let result =
        wamn_platform_identity::connections::write(&tx, org, operation, payload, &mut drops)
            .await
            .context("write connection configuration")?;
    tx.commit().await?;
    changes.publish(drops).await;
    let tx = client.transaction().await?;
    tx.batch_execute("SET LOCAL ROLE wamn_system").await?;
    for (project, env, mut target) in targets {
        let target_tx = target.transaction().await?;
        target_tx
            .batch_execute(&wamn_control_provision::bind_platform_principal_sql(
                wamn_control_provision::PlatformComponent::Provisioning,
            ))
            .await?;
        let projected =
            wamn_platform_identity::connections::project(&tx, &target_tx, org, &project, &env)
                .await
                .with_context(|| format!("project connection authority into {project}/{env}"))?;
        target_tx
            .commit()
            .await
            .with_context(|| format!("commit connection projection in {project}/{env}"))?;
        changes.publish(projected).await;
    }
    tx.commit().await?;
    Ok(result)
}

/// Import the legacy credential maps once for the named org.
pub async fn import(database_url: &str, org: &str, contents: &str) -> anyhow::Result<()> {
    let mut client = crate::environment::analyse::connect(database_url).await?;
    let tx = client.transaction().await?;
    tx.batch_execute("SET LOCAL ROLE wamn_system").await?;
    wamn_platform_identity::connections::import_credentials(&tx, org, contents).await?;
    tx.commit().await?;
    Ok(())
}

/// Rotate one connection credential without rewriting an older generation.
pub async fn rotate(
    database_url: &str,
    org: &str,
    id: &str,
    contents: &str,
) -> anyhow::Result<i64> {
    let mut client = crate::environment::analyse::connect(database_url).await?;
    let tx = client.transaction().await?;
    tx.batch_execute("SET LOCAL ROLE wamn_system").await?;
    let generation = wamn_platform_identity::connections::rotate(&tx, org, id, contents).await?;
    tx.commit().await?;
    Ok(generation)
}
