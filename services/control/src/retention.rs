//! Periodic maintenance of the org's project databases.

use sqlx::Connection as _;
use sqlx::postgres::PgConnectOptions;
use std::time::Duration;
use tokio::time::{Instant, timeout, timeout_at};
use wamn_authentication::administration::ApplicationTarget;
use wamn_registry::{OrgId, Triple, instance_suffix::InstanceSuffix};

#[cfg(test)]
mod tests;

const DATABASE_TIMEOUT: Duration = Duration::from_secs(5);

async fn prune_identities(connection: &mut sqlx::PgConnection) -> anyhow::Result<()> {
    let mut transaction = connection.begin().await?;
    crate::control_sql::prune_transaction_identities(&mut transaction).await?;
    let retired =
        wamn_connection::project_connections::retired_credentials(&mut transaction).await?;
    wamn_connection::credential_cleanup::collect(
        &wamn_connection::connection_credentials::KubernetesStore,
        retired,
    )
    .await;
    transaction.commit().await?;
    Ok(())
}

/// Prune expired identities and reconcile authority in each recorded project environment.
pub(crate) async fn identities<F>(
    system: &sqlx::PgPool,
    org: &OrgId,
    issuer: &str,
    login: impl Fn(Triple, InstanceSuffix) -> F,
) -> anyhow::Result<()>
where
    F: Future<Output = (Triple, anyhow::Result<PgConnectOptions>)>,
{
    for (triple, instance) in crate::control_sql::project_deployments(system, org).await? {
        let (triple, login) = login(triple, instance).await;
        let result = async {
            let login: wamn_postgres::ConnectionInput<'static> = login?.into();
            let deadline = Instant::now() + DATABASE_TIMEOUT;
            let mut project = timeout_at(deadline, wamn_postgres::connect(login)).await??;
            if let Err(error) = timeout_at(deadline, prune_identities(&mut project))
                .await
                .unwrap_or_else(|error| Err(error.into()))
            {
                tracing::warn!(%error, %triple, "transaction identity retention failed");
            }
            timeout(DATABASE_TIMEOUT, async {
                let mut system = system.acquire().await?;
                wamn_environment::environment_fill::fill(
                    &mut system,
                    &mut project,
                    issuer,
                    ApplicationTarget {
                        org: &triple.org,
                        project: &triple.project,
                        env: &triple.env,
                    },
                )
                .await
            })
            .await?
        }
        .await;
        if let Err(error) = result {
            tracing::warn!(%error, %triple, "project authority reconciliation failed");
        }
    }
    Ok(())
}
