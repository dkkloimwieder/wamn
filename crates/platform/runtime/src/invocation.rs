//! Invocation authority shared by PostgreSQL, HTTP, storage and tracing.

use std::sync::Arc;

use crate::plugins::connection_http::{
    ConnectionExecutionClosure, ConnectionInvocation, InvocationEntry,
};
use crate::plugins::wamn_postgres::{OperationTransaction, SessionIdentity, WamnPostgres};
use wamn_engine::flow_http_routing::AuthenticatedCaller;
use wamn_engine::invocation_scope::{InvocationScopes, ScopeLifetime};
use wamn_engine::invocation_trace::InvocationTrace;
use wamn_event_wire::Causation;

/// Immutable release and project-owned connection readers for one deployment.
#[derive(Debug)]
pub struct ReleaseContext {
    pub release: Arc<wamn_engine::release_manifest::LoadedRelease>,
    pub connections: Arc<crate::connection_facts::ConnectionFacts>,
    pub integrations: std::sync::OnceLock<Arc<crate::integrations::Integrations>>,
}

impl ReleaseContext {
    pub fn new(
        release: Arc<wamn_engine::release_manifest::LoadedRelease>,
        connections: Arc<crate::connection_facts::ConnectionFacts>,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            release.target().project_env_key == connections.project_env_key()
                && release.target().environment == connections.environment(),
            "connection release target mismatch"
        );
        Ok(Self {
            release,
            connections,
            integrations: std::sync::OnceLock::new(),
        })
    }
}

pub(crate) fn context(
    scopes: &InvocationScopes,
    scope: &str,
) -> anyhow::Result<Arc<ReleaseContext>> {
    use anyhow::Context as _;
    scopes
        .read::<Authority, _>(scope, |record| {
            let context = record
                .context
                .as_ref()
                .context("scope has no release authority")?;
            validate_context(&record.identity, context)?;
            Ok(Arc::clone(context))
        })
        .context("scope was revoked")?
}

pub(crate) fn validate_context(
    identity: &SessionIdentity,
    context: &ReleaseContext,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        identity.project_env_key == context.release.target().project_env_key
            && identity.project.as_deref() == Some(context.connections.project())
            && identity.release.as_ref()
                == Some(
                    &crate::plugins::wamn_postgres::ReleaseIdentity::from_loaded(&context.release)
                ),
        "invocation release authority mismatch"
    );
    Ok(())
}

/// Bind a host-selected deployment for a capability fixture.
#[cfg(any(test, feature = "test-util"))]
pub fn bind_test_context(
    postgres: &WamnPostgres,
    scope: &str,
    connections: Arc<crate::connection_facts::ConnectionFacts>,
    release: Arc<wamn_engine::release_manifest::LoadedRelease>,
) {
    let context = Arc::new(ReleaseContext::new(release, connections).expect("fixture context"));
    postgres.scopes.update::<Authority, _>(scope, |record| {
        record.identity.project_env_key = context.release.target().project_env_key.clone();
        record.identity.project = Some(context.connections.project().to_owned());
        record.identity.release = Some(
            crate::plugins::wamn_postgres::ReleaseIdentity::from_loaded(&context.release),
        );
        record.context = Some(context);
    });
}

/// The complete host authority of one scope. Workload binding fills the same record.
#[derive(Debug, Default)]
pub(crate) struct Authority {
    pub identity: SessionIdentity,
    pub context: Option<Arc<ReleaseContext>>,
    pub causation: Option<Causation>,
    pub invocation: Option<ConnectionInvocation>,
    pub transaction: Option<OperationTransaction>,
    pub wal_position: Option<String>,
    pub statements: crate::plugins::wamn_postgres::statements::StatementScope,
}

impl wamn_engine::invocation_scope::InvocationAuthority for Authority {
    fn release(&self) -> Option<Arc<wamn_engine::release_manifest::LoadedRelease>> {
        let context = self.context.as_ref()?;
        validate_context(&self.identity, context).ok()?;
        Some(Arc::clone(&context.release))
    }
}

/// An admitted call retains this owner through commit, replay and result signing.
#[derive(Debug)]
pub struct AuthorizedInvocation {
    lifetime: ScopeLifetime,
    pub caller: Option<AuthenticatedCaller>,
}

/// Whether authentication already established the principal's project membership.
#[derive(Debug, Clone, Copy)]
pub enum PrincipalVerification {
    Required,
    Authenticated,
}

/// Released facts needed to admit one call, shared by native and guest entries.
#[derive(Debug)]
pub struct InvocationRequest<'a> {
    pub scope: String,
    pub identity: SessionIdentity,
    pub context: Option<Arc<ReleaseContext>>,
    pub caller: Option<AuthenticatedCaller>,
    pub registered_operation: Option<&'a str>,
    pub causation: Option<Causation>,
    pub invocation: Option<ConnectionInvocation>,
    pub statements: crate::plugins::wamn_postgres::PreparedStatementSet,
    pub members: crate::plugins::wamn_postgres::PreparedStatementMembers,
    pub principal: PrincipalVerification,
}

impl AuthorizedInvocation {
    /// Install one authorized record. Cancellation drops the record even during a principal read.
    pub async fn enter(
        postgres: &WamnPostgres,
        request: InvocationRequest<'_>,
    ) -> anyhow::Result<Arc<Self>> {
        let InvocationRequest {
            scope,
            identity,
            context,
            caller,
            registered_operation,
            causation,
            invocation,
            statements,
            members,
            principal,
        } = request;
        wamn_engine::native_routes::authorize_registered_operation(
            caller.as_ref(),
            registered_operation,
        )?;
        crate::plugins::wamn_postgres::validate_identity(&identity)?;
        if let Some(invocation) = &invocation {
            validate_invocation(invocation)?;
        }
        if let Some(context) = &context {
            validate_context(&identity, context)?;
        }
        let lifetime = postgres.scopes.enter(
            scope,
            Authority {
                identity,
                context,
                causation,
                invocation,
                statements: crate::plugins::wamn_postgres::statements::StatementScope::active(
                    statements, members,
                ),
                ..Authority::default()
            },
            InvocationTrace::capture(),
        )?;
        let owner = Arc::new(Self { lifetime, caller });
        if matches!(principal, PrincipalVerification::Required) {
            postgres.verify_scope_principal(owner.id()).await?;
        }
        Ok(owner)
    }

    pub fn id(&self) -> &str {
        self.lifetime.id()
    }
}

pub(crate) fn identity(scopes: &InvocationScopes, scope: &str) -> Option<SessionIdentity> {
    scopes
        .read::<Authority, _>(scope, |record| record.identity.clone())
        .filter(|identity| !identity.project_env_key.is_empty())
}

pub(crate) fn validate_invocation(invocation: &ConnectionInvocation) -> anyhow::Result<()> {
    let entry_valid = match &invocation.entry {
        InvocationEntry::Route => !matches!(
            invocation.closure,
            ConnectionExecutionClosure::Candidate { .. }
        ),
        InvocationEntry::Wiring(position) => {
            !position.package_id.is_empty()
                && !position.wiring_id.is_empty()
                && position.wiring_version > 0
                && wamn_catalog::DefinitionHash::parse(&position.wiring_hash).is_ok()
                && !position.node_id.is_empty()
        }
    };
    anyhow::ensure!(
        !invocation.package_id.is_empty()
            && entry_valid
            && wamn_catalog::ArtifactHash::parse(invocation.component_digest.as_str()).is_ok(),
        "connection-http-invocation-invalid"
    );
    if let ConnectionExecutionClosure::Candidate {
        manifest_digest,
        environment,
        wiring_hash,
        component,
        interface_version,
        ..
    } = &invocation.closure
    {
        anyhow::ensure!(
            !manifest_digest.is_empty()
                && !environment.is_empty()
                && !component.is_empty()
                && !interface_version.is_empty()
                && wamn_catalog::DefinitionHash::parse(wiring_hash).is_ok(),
            "connection-http-candidate-closure-invalid"
        );
    }
    Ok(())
}
