//! The host half of one operation call.
//!
//! The released application, its plugins, and the policy that grants each
//! call's authority belong here. The call itself is
//! [`wamn_engine::operation::invoke_operation`], which the router driver and
//! the route path both call.

use std::fmt;
use std::sync::Arc;
use wamn_authority::identity::{ReleaseIdentity, SessionIdentity};
use wamn_connections::connection_facts::ConnectionFacts;
use wamn_sql::valid_runner;
use wamn_engine::release_manifest::LoadedRelease;

/// The process services that one released application runs on.
///
/// The process builds one host. The route path calls it directly, and the
/// router driver runs each wiring node on the same host, so both call into the
/// same loaded application.
pub struct OperationHost {
    /// Shared by every driver in this process, independently of fresh stores.
    pub context: Arc<wamn_connections::invocation::ReleaseContext>,
    pub release: Arc<LoadedRelease>,
    pub project: wamn_registry::ProjectId,
    schema: Option<String>,
    owner_prefix: String,
}

impl fmt::Debug for OperationHost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OperationHost")
            .field("project", &self.project)
            .field("schema", &self.schema)
            .field("owner_prefix", &self.owner_prefix)
            .finish_non_exhaustive()
    }
}

/// The process-owned facts of an [`OperationHost`], outside its services.
#[derive(Debug, Clone)]
pub struct OperationHostConfig {
    pub project: wamn_registry::ProjectId,
    pub schema: Option<String>,
    pub owner_prefix: String,
}

impl OperationHost {
    /// Bind one release to the process-owned services.
    pub fn new(
        facts: Arc<ConnectionFacts>,
        release: Arc<LoadedRelease>,
        target: OperationHostConfig,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            valid_runner(&target.owner_prefix),
            "invalid router owner {:?}: 1-128 chars of [A-Za-z0-9_-] required",
            target.owner_prefix
        );
        anyhow::ensure!(
            &target.project == facts.project(),
            "operation project authority mismatch"
        );
        Ok(Self {
            context: Arc::new(wamn_connections::invocation::ReleaseContext::new(
                Arc::clone(&release),
                facts,
            )?),
            release,
            project: target.project,
            schema: target.schema,
            owner_prefix: target.owner_prefix,
        })
    }

    /// The identity of the carried release, bound on every released call.
    pub fn release_identity(&self) -> ReleaseIdentity {
        ReleaseIdentity::from_loaded(&self.release)
    }

    /// The session identity of one call, before activation binds its principal.
    pub fn identity(
        &self,
        project_env_key: &wamn_registry::ProjectEnvKey,
        release: Option<ReleaseIdentity>,
    ) -> SessionIdentity {
        SessionIdentity {
            project_env_key: Some(project_env_key.to_owned()),
            project: Some(self.project.clone()),
            schema: self.schema.clone(),
            runner: Some(self.owner_prefix.clone()),
            role: None,
            // Activation binds the executing principal from the caller or
            // `platform`, so a nested call derives the same one.
            user_id: None,
            // Activation binds the operation token of `invocation`, so a
            // nested call binds its own operation.
            operation: None,
            // Activation binds the declared application of the component
            // that runs.
            application: None,
            release,
        }
    }

}

#[cfg(test)]
mod tests;
