//! Release and connection authority for one invocation scope.

use crate::connection::{ConnectionExecutionClosure, ConnectionInvocation, InvocationEntry};
use std::sync::Arc;
use wamn_engine::invocation_scope::InvocationScopes;

/// Immutable release and project-owned connection readers for one deployment.
#[derive(Debug)]
pub struct ReleaseContext {
    pub release: Arc<wamn_engine::release_manifest::LoadedRelease>,
    pub connections: Arc<crate::connection_facts::ConnectionFacts>,
}

impl ReleaseContext {
    pub fn new(
        release: Arc<wamn_engine::release_manifest::LoadedRelease>,
        connections: Arc<crate::connection_facts::ConnectionFacts>,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            &release.target().project_env_key == connections.project_env_key()
                && &release.target().environment == connections.environment(),
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
    pub context: Option<Arc<ReleaseContext>>,
    pub invocation: Option<ConnectionInvocation>,
}

impl wamn_engine::invocation_scope::InvocationAuthority for Scope {
    fn release(&self) -> Option<Arc<wamn_engine::release_manifest::LoadedRelease>> {
        self.context
            .as_ref()
            .map(|context| Arc::clone(&context.release))
    }
}

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

pub fn validate_invocation(invocation: &ConnectionInvocation) -> anyhow::Result<()> {
    let entry_valid = match &invocation.entry {
        InvocationEntry::Route => !matches!(
            invocation.closure,
            ConnectionExecutionClosure::Candidate { .. }
        ),
        InvocationEntry::Wiring(position) => {
            !position.wiring_id.is_empty()
                && position.wiring_version > 0
                && !position.node_id.is_empty()
        }
    };
    anyhow::ensure!(entry_valid, "connection-http-invocation-invalid");
    if let ConnectionExecutionClosure::Candidate {
        component,
        interface_version,
        ..
    } = &invocation.closure
    {
        anyhow::ensure!(
            !component.is_empty() && !interface_version.is_empty(),
            "connection-http-candidate-closure-invalid"
        );
    }
    Ok(())
}
