//! Invocation authority shared by PostgreSQL, HTTP, storage, logging and tracing.

use std::sync::Arc;

use crate::plugins::connection_http::{
    ConnectionExecutionClosure, ConnectionInvocation, InvocationEntry,
};
use crate::plugins::wamn_postgres::{OperationTransaction, SessionIdentity, WamnPostgres};
use wamn_engine::flow_http_routing::AuthenticatedCaller;
use wamn_engine::invocation_scope::{InvocationScopes, ScopeLifetime};
use wamn_engine::invocation_trace::InvocationTrace;
use wamn_event_wire::Causation;

/// The complete host authority of one scope. Workload binding fills the same record.
#[derive(Debug, Default)]
pub(crate) struct Authority {
    pub identity: SessionIdentity,
    pub causation: Option<Causation>,
    pub invocation: Option<ConnectionInvocation>,
    pub transaction: Option<OperationTransaction>,
    pub wal_position: Option<String>,
    pub class: Option<wamn_run_state::AuthorityClass>,
    pub statements: crate::plugins::wamn_postgres::statements::StatementScope,
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
        let lifetime = postgres.scopes.enter(
            scope,
            Authority {
                identity,
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
    let digest = invocation
        .component_digest
        .strip_prefix("sha256:")
        .unwrap_or_default();
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
            && digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
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
        let graph = wiring_hash.strip_prefix("sha256:").unwrap_or_default();
        anyhow::ensure!(
            !manifest_digest.is_empty()
                && !environment.is_empty()
                && !component.is_empty()
                && !interface_version.is_empty()
                && graph.len() == 64
                && graph
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "connection-http-candidate-closure-invalid"
        );
    }
    Ok(())
}
