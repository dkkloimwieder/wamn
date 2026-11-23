//! Application and connection authority for one invocation scope.

use crate::connection::ConnectionInvocation;
use std::sync::Arc;
use wamn_engine::invocation_scope::InvocationScopes;

/// Immutable application facts and project-owned connection readers for one deployment.
#[derive(Debug)]
pub struct ReleaseContext {
    pub release: Arc<wamn_application::application_config::ApplicationConfig>,
    pub connections: Arc<crate::connection_facts::ConnectionFacts>,
}

impl ReleaseContext {
    pub fn new(
        release: Arc<wamn_application::application_config::ApplicationConfig>,
        connections: Arc<crate::connection_facts::ConnectionFacts>,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            &wamn_registry::name::project_env_key(
                &release.org,
                &release.project,
                &release.environment
            ) == connections.project_env_key()
                && &release.environment == connections.environment(),
            "connection release target mismatch"
        );
        Ok(Self {
            release,
            connections,
        })
    }
}

/// Connection facts installed by the host after admission.
#[derive(Debug, Default)]
pub struct Scope {
    pub principal: Option<wamn_identity_protocol::PrincipalId>,
    pub context: Option<Arc<ReleaseContext>>,
    pub invocation: Option<ConnectionInvocation>,
}

impl wamn_application::invocation_authority::InvocationAuthority for Scope {}

/// Read the deployment selected by the host for this scope.
pub fn context(scopes: &InvocationScopes, scope: &str) -> anyhow::Result<Arc<ReleaseContext>> {
    use anyhow::Context as _;
    scopes
        .read::<Scope, _>(scope, |record| record.context.as_ref().map(Arc::clone))
        .context("scope was revoked")?
        .context("scope has no release authority")
}

/// Read the host-attested invocation while its scope remains active.
pub fn invocation(scopes: &InvocationScopes, scope: &str) -> Option<ConnectionInvocation> {
    scopes
        .read::<Scope, _>(scope, |record| record.invocation.clone())
        .flatten()
}
