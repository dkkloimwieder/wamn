//! Queue calls over the runtime's authorized transaction transport.

use super::{ReleaseIdentity, WamnPostgres};
use wamn_run_state::persistence::{self, LeaseTurn};
pub use wamn_run_state::persistence::{ProductionLeaseResult, ProductionWorkflow};
pub use wamn_run_state::run_store::{
    ProductionCompletion, ProductionCompletionResult, ProductionLeaseError,
    ProductionLeaseErrorType, ProductionLeaseRenewal, ProductionReapResult,
};

impl WamnPostgres {
    /// Whether a released invocation still retains this deployment.
    pub fn release_in_use(&self, release: &wamn_engine::release_manifest::LoadedRelease) -> bool {
        self.scopes.any::<crate::invocation::Authority>(|record| {
            record.invocation.is_some()
                && record
                    .context
                    .as_ref()
                    .is_some_and(|context| std::ptr::eq(context.release.as_ref(), release))
        })
    }

    /// Read restart pins through this project's executor credential.
    pub async fn unfinished_releases(
        &self,
        project: &str,
        project_env_key: &str,
        environment: &str,
    ) -> anyhow::Result<Vec<(String, String, String)>> {
        self.executor_read(project, project_env_key, async |connection| {
            Ok(persistence::unfinished_releases(connection, project_env_key, environment).await?)
        })
        .await
    }

    /// Load immutable catalog bytes through the owning project's executor credential.
    pub async fn release_bytes(
        &self,
        project: &str,
        project_env_key: &str,
        digest: &str,
    ) -> anyhow::Result<Vec<u8>> {
        self.executor_read(project, project_env_key, async |connection| {
            Ok(sqlx::query_scalar("SELECT canonical_bytes FROM catalog.releases WHERE project_env_key=$1 AND manifest_digest=$2")
                .bind(project_env_key).bind(digest).fetch_one(connection).await?)
        }).await
    }

    /// Run lease next under the bound executor identity.
    pub async fn lease_next(
        &self,
        component_id: &str,
        package_ids: &[String],
        environment: &str,
        lease_ttl_ms: i64,
    ) -> Result<ProductionLeaseResult, ProductionLeaseError> {
        let runner = self.runner_for(component_id).ok_or_else(|| {
            ProductionLeaseError::new(
                ProductionLeaseErrorType::Identity,
                "resolve runner",
                "component has no host-injected runner",
            )
        })?;
        let release = self.release_identity_for(component_id);
        let digest = release
            .as_ref()
            .map(ReleaseIdentity::manifest_digest)
            .map(|digest| digest.as_str());
        let result = self
            .executor_bound(component_id, async |connection| {
                persistence::lease_next(
                    connection,
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
        environment: &str,
        grace_ms: i64,
    ) -> Result<ProductionReapResult, ProductionLeaseError> {
        let result = self
            .executor_bound(component_id, async |connection| {
                persistence::reap_exhausted(connection, package_ids, environment, grace_ms).await
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
        let runner = self.runner_for(component_id).ok_or_else(|| {
            ProductionLeaseError::new(
                ProductionLeaseErrorType::Identity,
                "resolve runner",
                "component has no host-injected runner",
            )
        })?;
        let result = self
            .executor_bound(component_id, async |connection| {
                persistence::renew(connection, run_id, &runner, lease_generation, lease_ttl_ms)
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
        let runner = self.runner_for(component_id).ok_or_else(|| {
            ProductionLeaseError::new(
                ProductionLeaseErrorType::Identity,
                "resolve runner",
                "component has no host-injected runner",
            )
        })?;
        let result = self
            .executor_bound(component_id, async |connection| {
                persistence::complete(connection, run_id, &runner, lease_generation, completion)
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
        let runner = self.runner_for(component_id).ok_or_else(|| {
            ProductionLeaseError::new(
                ProductionLeaseErrorType::Identity,
                "resolve runner",
                "component has no host-injected runner",
            )
        })?;
        let result = self
            .executor_bound(component_id, async |connection| {
                persistence::record_deadline_adjustments(
                    connection,
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
        let runner = self.runner_for(component_id).ok_or_else(|| {
            ProductionLeaseError::new(
                ProductionLeaseErrorType::Identity,
                "resolve runner",
                "component has no host-injected runner",
            )
        })?;
        let result = self
            .executor_bound(component_id, async |connection| {
                persistence::release_lease(connection, run_id, &runner, lease_generation).await
            })
            .await?;
        Ok(result)
    }

    /// Run pinned backlog under the bound executor identity.
    pub async fn pinned_backlog(
        &self,
        component_id: &str,
        package_ids: &[String],
        environment: &str,
    ) -> Result<bool, ProductionLeaseError> {
        let release = self.release_identity_for(component_id);
        let digest = release
            .as_ref()
            .map(ReleaseIdentity::manifest_digest)
            .map(|digest| digest.as_str());
        let Some(digest) = digest else {
            return Ok(false);
        };
        let result = self
            .executor_bound(component_id, async |connection| {
                persistence::pinned_backlog(connection, package_ids, environment, digest).await
            })
            .await?;
        Ok(result)
    }
}
