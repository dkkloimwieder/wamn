//! Restore host-captured tracing context when native dispatch polls host callbacks.
//!
//! This carrier exposes no guest interface or configuration. Its scope keys
//! identify existing host invocations; a trace supplies no authorization.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use tracing::{Instrument as _, instrument::WithSubscriber as _};
use wash_runtime::engine::ctx::ActiveCtx;
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wit::WitWorld;

/// Internal plugin identity for host invocation trace context.
pub const INVOCATION_TRACES_ID: &str = "wamn:invocation-traces";

/// One invocation's tracing span and the subscriber that owns it.
#[derive(Clone, Debug)]
pub struct InvocationTrace {
    span: tracing::Span,
    dispatcher: tracing::Dispatch,
}

impl InvocationTrace {
    /// Capture the tracing context at a host invocation boundary.
    #[must_use]
    pub fn capture() -> Self {
        Self {
            span: tracing::Span::current(),
            dispatcher: tracing::dispatcher::get_default(Clone::clone),
        }
    }

    /// Restore this context for one synchronous host callback.
    pub fn in_scope<R>(&self, call: impl FnOnce() -> R) -> R {
        tracing::dispatcher::with_default(&self.dispatcher, || self.span.in_scope(call))
    }

    /// Restore this context only while the host callback future is polled.
    pub async fn run<F: Future>(self, future: F) -> F::Output {
        future
            .instrument(self.span)
            .with_subscriber(self.dispatcher)
            .await
    }
}

/// Tracing contexts owned by the host's currently active invocation scopes.
#[derive(Debug, Default)]
pub struct InvocationTraces {
    scopes: Mutex<HashMap<String, InvocationTrace>>,
}

impl InvocationTraces {
    /// Associate a trace with a fresh host scope after guest initialization.
    pub fn bind(&self, scope: &str, trace: InvocationTrace) {
        self.scopes
            .lock()
            .expect("invocation traces lock poisoned")
            .insert(scope.to_owned(), trace);
    }

    /// Read the trace of a scope that the host still owns.
    #[must_use]
    pub fn get(&self, scope: &str) -> Option<InvocationTrace> {
        self.scopes
            .lock()
            .expect("invocation traces lock poisoned")
            .get(scope)
            .cloned()
    }

    /// Return whether all invocation tracing contexts have been released.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.scopes
            .lock()
            .expect("invocation traces lock poisoned")
            .is_empty()
    }

    /// Release a scope's trace when its existing invocation guard ends.
    pub fn revoke(&self, scope: &str) {
        self.scopes
            .lock()
            .expect("invocation traces lock poisoned")
            .remove(scope);
    }
}

#[async_trait::async_trait]
impl HostPlugin for InvocationTraces {
    fn id(&self) -> &'static str {
        INVOCATION_TRACES_ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::new(),
            exports: HashSet::new(),
        }
    }
}

/// Read native invocation context, preserving ambient tracing on other host paths.
#[must_use]
pub fn invocation_trace(active: &ActiveCtx<'_>) -> InvocationTrace {
    active
        .ctx
        .try_get_plugin::<InvocationTraces>(INVOCATION_TRACES_ID)
        .ok()
        .and_then(|traces| traces.get(&active.ctx.component_id))
        .unwrap_or_else(InvocationTrace::capture)
}
