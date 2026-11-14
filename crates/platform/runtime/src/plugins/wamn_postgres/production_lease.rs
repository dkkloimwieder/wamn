//! Queue calls over the runtime's authorized transaction transport.

use super::{ReleaseIdentity, WamnPostgres};
use wamn_run_state::persistence::{self, LeaseTurn};
pub use wamn_run_state::persistence::{ProductionLeaseResult, ProductionWorkflow};
pub use wamn_run_state::run_store::{
    ProductionCompletion, ProductionCompletionResult, ProductionLeaseError,
    ProductionLeaseErrorType, ProductionLeaseRenewal, ProductionReapResult,
};

impl WamnPostgres {
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
