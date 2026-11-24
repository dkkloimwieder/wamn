//! PostgreSQL registry operations for environment deployment.

use anyhow::Context as _;
use wamn_registry::deployment::DeploymentStore as _;
use wamn_registry::{Triple, instance_suffix::InstanceSuffix};

/// Read one project-env's stored instance suffix from the registry.
pub async fn read_project_env_instance(
    client: &mut sqlx::PgConnection,
    triple: &Triple,
) -> anyhow::Result<InstanceSuffix> {
    let mut transaction = crate::identity::transaction::read_connection(
        client,
        &crate::PlatformComponent::Provisioning.principal_id(),
    )
    .await?;
    let result = async {
        let stored = crate::Registry::new(&mut transaction)
            .instance_suffix(triple)
            .await
            .context("read project deployment")?
            .with_context(|| format!("project-env {triple} is not recorded"))?;
        Ok(stored)
    }
    .await;
    transaction.commit().await?;
    result
}

/// Record the project and its deployment in the caller's reservation transaction.
///
/// Returns the stored instance suffix. A new deployment uses `candidate`.
#[expect(clippy::too_many_arguments)]
pub async fn record_project_env(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    triple: &Triple,
    readiness_budget_seconds: i32,
    drain_bound_seconds: i32,
    target: &str,
    secret_name: &str,
    secret_namespace: Option<&str>,
    candidate: &InstanceSuffix,
    disposable: Option<bool>,
) -> anyhow::Result<InstanceSuffix> {
    crate::Registry::new(transaction)
        .record_project(&triple.org, &triple.project)
        .await
        .context("record project")?;
    let target_id = crate::Registry::new(transaction)
        .target_id(target)
        .await
        .context("read deployment target")?
        .with_context(|| format!("target {target:?} is not registered"))?;
    crate::Registry::new(transaction)
        .record(&wamn_registry::deployment::Registration {
            triple,
            secret_name,
            secret_namespace,
            instance_suffix: candidate,
            disposable,
            readiness_budget_seconds,
            drain_bound_seconds,
            target_id: &target_id,
        })
        .await
        .context("record project deployment")
}

/// Require the environment's registry coordinate to exist.
pub async fn require_connected(
    client: &mut sqlx::PgConnection,
    triple: &Triple,
) -> anyhow::Result<()> {
    let present = crate::Registry::new(client).exists(triple).await?;
    anyhow::ensure!(present, "project environment is not recorded");
    Ok(())
}
