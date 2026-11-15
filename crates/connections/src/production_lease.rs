//! Queue calls over the runtime's authorized transaction transport.

use crate::database::ConnectionGuard;
use wamn_run_state::persistence::{self, LeaseTurn};
pub use wamn_run_state::persistence::{ProductionLeaseResult, ProductionWorkflow};
pub use wamn_run_state::run_store::{
    ProductionCompletion, ProductionCompletionResult, ProductionLeaseError,
    ProductionLeaseErrorType, ProductionLeaseRenewal, ProductionReapResult,
};

/// The existing database transport supplies the bound executor identity.
pub trait ExecutorDatabase: Send + Sync {
    /// Open a transaction with the identity already bound to this component.
    fn bound_connection(
        &self,
        component: &str,
    ) -> impl std::future::Future<Output = Result<ConnectionGuard, ProductionLeaseError>> + Send;
    /// The project database identity bound to this component.
    fn project_env_key(&self, component: &str) -> Option<wamn_registry::ProjectEnvKey>;
    /// The host-bound queue runner for this component.
    fn runner(&self, component: &str) -> Option<String>;
    /// The release that this component serves.
    fn manifest_digest(&self, component: &str) -> Option<wamn_digest::Digest>;
}

/// Queue leases over the host's existing authorized connections.
pub struct ProductionLeases<'a, D> {
    database: &'a D,
}

impl<D> std::fmt::Debug for ProductionLeases<'_, D> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProductionLeases")
            .finish_non_exhaustive()
    }
}

impl<'a, D: ExecutorDatabase> ProductionLeases<'a, D> {
    /// Borrow the host's existing executor transport.
    pub fn new(database: &'a D) -> Self {
        Self { database }
    }

    async fn executor_bound<T>(
        &self,
        component: &str,
        work: impl AsyncFnOnce(
            &mut sqlx::PgConnection,
            &wamn_registry::ProjectEnvKey,
        ) -> Result<T, ProductionLeaseError>,
    ) -> Result<T, ProductionLeaseError> {
        let project_env_key = self.database.project_env_key(component).ok_or_else(|| {
            ProductionLeaseError::new(
                ProductionLeaseErrorType::Identity,
                "resolve executor identity",
                "component has no host-injected project",
            )
        })?;
        let mut connection = self.database.bound_connection(component).await?;
        match work(connection.connection(), &project_env_key).await {
            Ok(value) => {
                sqlx::raw_sql("COMMIT")
                    .execute(connection.connection())
                    .await
                    .map_err(|error| {
                        ProductionLeaseError::postgres("commit executor transaction", &error)
                    })?;
                connection.repool();
                Ok(value)
            }
            Err(error) => {
                if sqlx::raw_sql("ROLLBACK")
                    .execute(connection.connection())
                    .await
                    .is_ok()
                {
                    connection.repool();
                }
                Err(error)
            }
        }
    }

    /// Run lease next under the bound executor identity.
    pub async fn lease_next(
        &self,
        component_id: &str,
        package_ids: &[String],
        environment: &wamn_registry::EnvName,
        lease_ttl_ms: i64,
    ) -> Result<ProductionLeaseResult, ProductionLeaseError> {
        let runner = self.database.runner(component_id).ok_or_else(|| {
            ProductionLeaseError::new(
                ProductionLeaseErrorType::Identity,
                "resolve runner",
                "component has no host-injected runner",
            )
        })?;
        let digest = self.database.manifest_digest(component_id);
        let digest = digest.as_ref();
        let result = self
            .executor_bound(component_id, async |connection, project_env_key| {
                persistence::lease_next(
                    connection,
                    project_env_key,
                    &runner,
                    package_ids,
                    environment,
                    lease_ttl_ms,
                    digest,
                )
                .await
            })
            .await?;
        match result {
            LeaseTurn::Leased(result) => Ok(result),
            LeaseTurn::GrantRefused(error) => Err(error),
        }
    }

    /// Run reap exhausted under the bound executor identity.
    pub async fn reap_exhausted(
        &self,
        component_id: &str,
        package_ids: &[String],
        environment: &wamn_registry::EnvName,
        grace_ms: i64,
    ) -> Result<ProductionReapResult, ProductionLeaseError> {
        let result = self
            .executor_bound(component_id, async |connection, project_env_key| {
                persistence::reap_exhausted(
                    connection,
                    project_env_key,
                    package_ids,
                    environment,
                    grace_ms,
                )
                .await
            })
            .await?;
        Ok(result)
    }

    /// Run renew under the bound executor identity.
    pub async fn renew(
        &self,
        component_id: &str,
        run_id: &str,
        lease_generation: i64,
        lease_ttl_ms: i64,
    ) -> Result<ProductionLeaseRenewal, ProductionLeaseError> {
        let runner = self.database.runner(component_id).ok_or_else(|| {
            ProductionLeaseError::new(
                ProductionLeaseErrorType::Identity,
                "resolve runner",
                "component has no host-injected runner",
            )
        })?;
        let result = self
            .executor_bound(component_id, async |connection, project_env_key| {
                persistence::renew(
                    connection,
                    project_env_key,
                    run_id,
                    &runner,
                    lease_generation,
                    lease_ttl_ms,
                )
                .await
            })
            .await?;
        Ok(result)
    }

    /// Run complete under the bound executor identity.
    pub async fn complete(
        &self,
        component_id: &str,
        run_id: &str,
        lease_generation: i64,
        completion: &ProductionCompletion,
    ) -> Result<ProductionCompletionResult, ProductionLeaseError> {
        let runner = self.database.runner(component_id).ok_or_else(|| {
            ProductionLeaseError::new(
                ProductionLeaseErrorType::Identity,
                "resolve runner",
                "component has no host-injected runner",
            )
        })?;
        let result = self
            .executor_bound(component_id, async |connection, project_env_key| {
                persistence::complete(
                    connection,
                    project_env_key,
                    run_id,
                    &runner,
                    lease_generation,
                    completion,
                )
                .await
            })
            .await?;
        Ok(result)
    }

    /// Run record deadline adjustments under the bound executor identity.
    pub async fn record_deadline_adjustments(
        &self,
        component_id: &str,
        run_id: &str,
        lease_generation: i64,
        adjustments: &serde_json::Value,
    ) -> Result<bool, ProductionLeaseError> {
        let runner = self.database.runner(component_id).ok_or_else(|| {
            ProductionLeaseError::new(
                ProductionLeaseErrorType::Identity,
                "resolve runner",
                "component has no host-injected runner",
            )
        })?;
        let result = self
            .executor_bound(component_id, async |connection, project_env_key| {
                persistence::record_deadline_adjustments(
                    connection,
                    project_env_key,
                    run_id,
                    &runner,
                    lease_generation,
                    adjustments,
                )
                .await
            })
            .await?;
        Ok(result)
    }

    /// Run release lease under the bound executor identity.
    pub async fn release_lease(
        &self,
        component_id: &str,
        run_id: &str,
        lease_generation: i64,
    ) -> Result<bool, ProductionLeaseError> {
        let runner = self.database.runner(component_id).ok_or_else(|| {
            ProductionLeaseError::new(
                ProductionLeaseErrorType::Identity,
                "resolve runner",
                "component has no host-injected runner",
            )
        })?;
        let result = self
            .executor_bound(component_id, async |connection, project_env_key| {
                persistence::release_lease(
                    connection,
                    project_env_key,
                    run_id,
                    &runner,
                    lease_generation,
                )
                .await
            })
            .await?;
        Ok(result)
    }

    /// Run pinned backlog under the bound executor identity.
    pub async fn pinned_backlog(
        &self,
        component_id: &str,
        package_ids: &[String],
        environment: &wamn_registry::EnvName,
    ) -> Result<bool, ProductionLeaseError> {
        let digest = self.database.manifest_digest(component_id);
        let digest = digest.as_ref();
        let Some(digest) = digest else {
            return Ok(false);
        };
        let result = self
            .executor_bound(component_id, async |connection, project_env_key| {
                persistence::pinned_backlog(
                    connection,
                    project_env_key,
                    package_ids,
                    environment,
                    digest,
                )
                .await
            })
            .await?;
        Ok(result)
    }
}
