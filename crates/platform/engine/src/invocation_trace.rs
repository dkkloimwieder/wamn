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

/// Request context retained by the HTTP Store, including anonymous calls.
#[derive(Debug)]
pub struct RequestTrace(pub InvocationTrace);

/// Capture incoming HTTP trace fields at the authentication import boundary.
pub fn request_trace(headers: &[(String, String)]) -> InvocationTrace {
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;
    struct Headers<'a>(&'a [(String, String)]);
    impl opentelemetry::propagation::Extractor for Headers<'_> {
        fn get(&self, key: &str) -> Option<&str> {
            self.0
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(key))
                .map(|(_, value)| value.as_str())
        }
        fn keys(&self) -> Vec<&str> {
            self.0.iter().map(|(name, _)| name.as_str()).collect()
        }
    }
    let parent = opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.extract(&Headers(headers))
    });
    let span = tracing::info_span!("wamn.operation.http");
    let _ = span.set_parent(parent);
    span.in_scope(InvocationTrace::capture)
}

/// The coordinates of one `wamn.component.invoke` span.
///
/// A route has no wiring position, so it names the empty wiring, version 0
/// and the empty node, the same as its effect spans.
#[derive(Debug)]
pub struct InvocationSite<'a> {
    pub env_key: &'a str,
    pub project: &'a str,
    pub environment: &'a str,
    pub wiring_id: &'a str,
    pub wiring_version: u32,
    pub node_id: &'a str,
    pub input_port: Option<&'a str>,
    pub operation: &'a str,
    pub component_digest: &'a str,
    pub caller: Option<&'a crate::flow_http_routing::AuthenticatedCaller>,
}

/// Open one `wamn.component.invoke` span, under `remote_parent` when given.
#[must_use]
pub fn invocation_span(
    site: &InvocationSite<'_>,
    remote_parent: Option<&opentelemetry::Context>,
) -> tracing::Span {
    let span = tracing::info_span!(
        target: "wamn::router",
        "wamn.component.invoke",
        wamn.env_key = %site.env_key,
        wamn.project = %site.project,
        wamn.environment = %site.environment,
        wamn.wiring_id = %site.wiring_id,
        wamn.wiring_version = site.wiring_version,
        wamn.component_digest = %site.component_digest,
        wamn.node_id = %site.node_id,
        wamn.operation = %site.operation,
        wamn.caller_principal_id = tracing::field::Empty,
        wamn.caller_credential_type = tracing::field::Empty,
        wamn.input_port = tracing::field::Empty,
        wamn.attachment_id = tracing::field::Empty,
    );
    if let Some(caller) = site.caller {
        span.record("wamn.caller_principal_id", caller.principal_id());
        span.record(
            "wamn.caller_credential_type",
            match caller.credential_type() {
                crate::flow_http_routing::CredentialType::Pat => "pat",
                crate::flow_http_routing::CredentialType::Session => "session",
                crate::flow_http_routing::CredentialType::QueuedService => "queued-service",
                crate::flow_http_routing::CredentialType::Connection => "connection",
            },
        );
    }
    if let Some(input_port) = site.input_port {
        span.record("wamn.input_port", input_port);
    }
    if let Some(parent) = remote_parent {
        // A tracing subscriber without the OTel layer is the supported
        // no-export mode. `set_parent` may then refuse; the span still records
        // locally and no invocation is affected.
        let _ = tracing_opentelemetry::OpenTelemetrySpanExt::set_parent(&span, parent.clone());
    }
    span
}

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
pub fn invocation_trace(active: &mut ActiveCtx<'_>) -> InvocationTrace {
    active
        .ctx
        .try_get_plugin::<InvocationTraces>(INVOCATION_TRACES_ID)
        .ok()
        .and_then(|traces| traces.get(&active.ctx.component_id))
        .or_else(|| {
            active.table.iter_mut().find_map(|value| {
                value
                    .downcast_ref::<RequestTrace>()
                    .map(|trace| trace.0.clone())
            })
        })
        .unwrap_or_else(InvocationTrace::capture)
}
