//! SQL identity and transaction authority for one invocation scope.

use std::sync::Arc;

use crate::{OperationTransaction, SessionIdentity, WamnPostgres};
use wamn_connection::connection::ConnectionInvocation;
use wamn_connection::invocation::{ReleaseContext, Scope};
use wamn_engine::ingress::AuthenticatedCaller;
use wamn_engine::invocation_scope::{InvocationAuthority, InvocationScopes, ScopeLifetime};
use wamn_engine::invocation_trace::InvocationTrace;
use wamn_event_wire::Causation;

pub fn validate_context(
    identity: &SessionIdentity,
    context: &ReleaseContext,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        identity.project_env_key.as_ref()
            == Some(&wamn_registry::name::project_env_key(
                &context.release.org,
                &context.release.project,
                &context.release.environment
            ))
            && identity.project.as_ref() == Some(context.connections.project())
            && identity.application.as_ref() == Some(&context.release.application),
        "invocation application authority mismatch"
    );
    Ok(())
}

/// Bind a host-selected deployment for a host service fixture.
#[cfg(any(test, feature = "test-util"))]
pub fn bind_test_context(
    postgres: &WamnPostgres,
    scope: &str,
    connections: Arc<wamn_connection::connection_facts::ConnectionFacts>,
    release: Arc<wamn_catalog::application_config::ApplicationConfig>,
) {
    let context = Arc::new(ReleaseContext::new(release, connections).expect("fixture context"));
    postgres.scopes.update::<Authority, _>(scope, |record| {
        record.identity.project_env_key = Some(wamn_registry::name::project_env_key(
            &context.release.org,
            &context.release.project,
            &context.release.environment,
        ));
        record.identity.project = Some(context.connections.project().to_owned());
        record.identity.application = Some(context.release.application.clone());
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

impl wamn_engine::invocation_scope::InvocationAuthority for Authority {}

/// An admitted call retains this owner through commit, replay and result signing.
#[derive(Debug)]
pub struct AuthorizedInvocation {
    lifetime: ScopeLifetime,
    pub caller: Option<AuthenticatedCaller>,
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
}

impl AuthorizedInvocation {
    /// Install both authority records for the caller admitted by authentication.
    pub fn enter(
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
        } = request;
        wamn_engine::native_routes::authorize_registered_operation(
            caller.as_ref(),
            registered_operation,
        )?;
        crate::validate_identity(&identity)?;
        if let Some(context) = &context {
            validate_context(&identity, context)?;
        }
        let principal = identity.user_id;
        let mut authorities = vec![Box::new(Authority {
            identity,
            causation,
            read,
            ..Authority::default()
        }) as Box<dyn InvocationAuthority>];
        if context.is_some() || invocation.is_some() {
            authorities.push(Box::new(Scope {
                principal,
                context,
                invocation,
            }));
        }
        let lifetime = postgres
            .scopes
            .enter(scope, authorities, InvocationTrace::capture())?;
        Ok(Arc::new(Self { lifetime, caller }))
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
