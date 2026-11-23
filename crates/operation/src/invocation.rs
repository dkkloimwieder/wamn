//! SQL identity and transaction authority for one invocation scope.

use std::sync::Arc;

use crate::scope_registry::{Authority, validate_context, validate_identity};
use wamn_application::ingress::AuthenticatedCaller;
use wamn_application::invocation_authority::InvocationAuthority;
use wamn_application::session_identity::SessionIdentity;
use wamn_connection::connection::ConnectionInvocation;
use wamn_connection::invocation::{ReleaseContext, Scope};
use wamn_engine::invocation_scope::{InvocationScopes, ScopeLifetime};
use wamn_engine::invocation_trace::InvocationTrace;
use wamn_event::Causation;

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
    pub registered_operation: Option<&'a wamn_package::registered_operation::RegisteredOperation>,
    pub causation: Option<Causation>,
    pub invocation: Option<ConnectionInvocation>,
    pub read: bool,
}

impl AuthorizedInvocation {
    /// Install both authority records for the caller admitted by authentication.
    pub fn enter(
        scopes: &Arc<InvocationScopes>,
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
        wamn_application::native_routes::authorize_registered_operation(
            caller.as_ref(),
            registered_operation,
        )?;
        validate_identity(&identity)?;
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
        let lifetime = scopes
            .enter(scope, authorities, InvocationTrace::capture())?;
        Ok(Arc::new(Self { lifetime, caller }))
    }

    pub fn id(&self) -> &str {
        self.lifetime.id()
    }
}

