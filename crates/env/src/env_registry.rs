//! PostgreSQL registry operations for environment deployment.

use crate::registry::deployment;
use anyhow::Context as _;
use wamn_registry::{Triple, instance_suffix::InstanceSuffix};

/// Read one project-env's stored instance suffix from the registry.
pub async fn read_project_env_instance(
    client: &mut sqlx::PgConnection,
    triple: &Triple,
) -> anyhow::Result<InstanceSuffix> {
    let mut transaction = wamn_identity::identity::transaction::read_connection(
        client,
        &wamn_principal::PlatformComponent::Provisioning.principal_id(),
    )
    .await?;
    let result = async {
        let stored = deployment::instance_suffix(&mut transaction, triple)
            .await
            .context("read project deployment")?
            .with_context(|| format!("project-env {triple} is not recorded"))?;
        Ok(stored)
    }
    .await;
    transaction.commit().await?;
    result
}

/// Record the project and its deployment in a provisioned environment.
///
/// Returns the stored instance suffix. A new or re-added deployment uses `candidate`.
pub async fn record_project_env(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    triple: &Triple,
    target: &str,
    secret_name: &str,
    secret_namespace: Option<&str>,
    candidate: &InstanceSuffix,
    disposable: Option<bool>,
) -> anyhow::Result<InstanceSuffix> {
    deployment::require_provisioned(transaction, &triple.org, &triple.env).await?;
    deployment::record_project(transaction, &triple.org, &triple.project)
        .await
        .context("record project")?;
    let active: bool = sqlx::query_scalar(wamn_identity::identity_sql::PROJECT_IN_ORG_SQL)
        .bind(triple.org.as_str())
        .bind(triple.project.as_str())
        .fetch_one(&mut **transaction)
        .await
        .context("read project activity")?;
    anyhow::ensure!(
        active,
        "project {}/{} is inactive",
        triple.org,
        triple.project
    );
    let target_id = deployment::target_id(transaction, target)
        .await
        .context("read deployment target")?
        .with_context(|| format!("target {target:?} is not registered"))?;
    deployment::record(
        transaction,
        &wamn_registry::deployment::Registration {
            triple,
            secret_name,
            secret_namespace,
            instance_suffix: candidate,
            disposable,
            target_id: &target_id,
        },
    )
    .await
    .context("record project deployment")
}

/// Require the environment's registry coordinate to exist.
pub async fn require_connected(
    client: &mut sqlx::PgConnection,
    triple: &Triple,
) -> anyhow::Result<()> {
    let present = deployment::exists(client, triple).await?;
    anyhow::ensure!(present, "project environment is not recorded");
    Ok(())
}
