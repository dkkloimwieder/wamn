//! SQL identity and transaction authority for one invocation scope.

use std::sync::Arc;

use crate::{OperationTransaction, SessionIdentity, WamnPostgres};
use wamn_connections::connection::ConnectionInvocation;
use wamn_connections::invocation::{ReleaseContext, Scope, validate_invocation};
use wamn_engine::flow_http_routing::AuthenticatedCaller;
use wamn_engine::invocation_scope::{InvocationAuthority, InvocationScopes, ScopeLifetime};
use wamn_engine::invocation_trace::InvocationTrace;
use wamn_event_wire::Causation;

pub fn validate_context(
    identity: &SessionIdentity,
    context: &ReleaseContext,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        identity.project_env_key.as_ref() == Some(&context.release.target().project_env_key)
            && identity.project.as_deref() == Some(context.connections.project())
            && identity.release.as_ref()
                == Some(&wamn_authority::identity::ReleaseIdentity::from_loaded(
                    &context.release
                )),
        "invocation release authority mismatch"
    );
    Ok(())
}

/// Bind a host-selected deployment for a host service fixture.
#[cfg(any(test, feature = "test-util"))]
pub fn bind_test_context(
    postgres: &WamnPostgres,
    scope: &str,
    connections: Arc<wamn_connections::connection_facts::ConnectionFacts>,
    release: Arc<wamn_engine::release_manifest::LoadedRelease>,
) {
    let context = Arc::new(ReleaseContext::new(release, connections).expect("fixture context"));
    postgres.scopes.update::<Authority, _>(scope, |record| {
        record.identity.project_env_key = Some(context.release.target().project_env_key.clone());
        record.identity.project = Some(context.connections.project().to_owned());
        record.identity.release = Some(wamn_authority::identity::ReleaseIdentity::from_loaded(
            &context.release,
        ));
    });
    postgres
        .scopes
        .update::<Scope, _>(scope, |record| record.context = Some(context));
}

/// SQL authority retained through commit and result signing.
#[derive(Debug, Default)]
pub struct Authority {
    pub identity: SessionIdentity,
    pub causation: Option<Causation>,
    pub transaction: Option<OperationTransaction>,
    pub wal_position: Option<String>,
    pub(crate) read: bool,
}

impl wamn_engine::invocation_scope::InvocationAuthority for Authority {
    fn release(&self) -> Option<Arc<wamn_engine::release_manifest::LoadedRelease>> {
        None
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
    pub registered_operation: Option<&'a wamn_catalog::registered_operation::RegisteredOperation>,
    pub causation: Option<Causation>,
    pub invocation: Option<ConnectionInvocation>,
    pub read: bool,
    pub principal: PrincipalVerification,
}

impl AuthorizedInvocation {
    /// Install both authority records. Cancellation drops them even during a principal read.
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
            read,
            principal,
        } = request;
        wamn_engine::native_routes::authorize_registered_operation(
            caller.as_ref(),
            registered_operation,
        )?;
        crate::validate_identity(&identity)?;
        if let Some(invocation) = &invocation {
            validate_invocation(invocation)?;
        }
        if let Some(context) = &context {
            validate_context(&identity, context)?;
        }
        let lifetime = postgres.scopes.enter(
            scope,
            [
                Box::new(Authority {
                    identity,
                    causation,
                    read,
                    ..Authority::default()
                }) as Box<dyn InvocationAuthority>,
                Box::new(Scope {
                    context,
                    invocation,
                }),
            ],
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
        .filter(|identity| identity.project_env_key.is_some())
}
