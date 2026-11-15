//! Restore host-captured tracing context when native dispatch polls host callbacks.
//!
//! This carrier exposes no guest interface or configuration. Its scope keys
//! identify existing host invocations; a trace supplies no authorization.

use tracing::{Instrument as _, instrument::WithSubscriber as _};
use wash_runtime::engine::ctx::ActiveCtx;

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
    pub project_env_key: &'a str,
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
        wamn.project_env_key = %site.project_env_key,
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

/// Read the Store request trace before the component invocation trace.
#[must_use]
pub fn invocation_trace(active: &mut ActiveCtx<'_>) -> InvocationTrace {
    use crate::invocation_scope::{INVOCATION_SCOPES_ID, InvocationScopes};
    active
        .ctx
        .try_get_plugin::<InvocationScopes>(INVOCATION_SCOPES_ID)
        .ok()
        .and_then(|scopes| {
            scopes
                .trace(&active.ctx.id)
                .or_else(|| scopes.trace(&active.ctx.component_id))
        })
        .unwrap_or_else(InvocationTrace::capture)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use wash_runtime::engine::ctx::Ctx;
    use wash_runtime::plugin::HostPlugin;
    use wash_runtime::wasmtime::component::ResourceTable;

    use super::*;
    use crate::invocation_scope::{INVOCATION_SCOPES_ID, InvocationScopes};
    use crate::release_manifest::LoadedRelease;

    #[test]
    fn overlapping_stores_keep_their_request_trace_and_release_it_on_drop() {
        tracing::subscriber::with_default(tracing_subscriber::registry(), || {
            let scopes = Arc::new(InvocationScopes::default());
            let release = Arc::new(LoadedRelease::control_root());
            let component = tracing::info_span!("component");
            component.in_scope(|| scopes.bind_release("component", Arc::clone(&release)));
            let store_context = || {
                Ctx::builder("workload", "component")
                    .with_plugins(HashMap::from([(
                        INVOCATION_SCOPES_ID,
                        Arc::clone(&scopes) as Arc<dyn HostPlugin + Send + Sync>,
                    )]))
                    .build()
            };
            let mut first = store_context();
            let mut second = store_context();
            let first_span = tracing::info_span!("first_request");
            let second_span = tracing::info_span!("second_request");
            let mut first_table = ResourceTable::new();
            let mut second_table = ResourceTable::new();
            first_table
                .push(
                    scopes
                        .enter_trace(
                            first.id.to_string(),
                            first_span.in_scope(InvocationTrace::capture),
                        )
                        .unwrap(),
                )
                .unwrap();
            second_table
                .push(
                    scopes
                        .enter_trace(
                            second.id.to_string(),
                            second_span.in_scope(InvocationTrace::capture),
                        )
                        .unwrap(),
                )
                .unwrap();
            let current = |ctx: &mut Ctx, table: &mut ResourceTable| {
                invocation_trace(&mut ActiveCtx { ctx, table })
                    .in_scope(|| tracing::Span::current().id())
            };
            assert_eq!(current(&mut first, &mut first_table), first_span.id());
            assert_eq!(current(&mut second, &mut second_table), second_span.id());
            drop(first_table);
            assert!(scopes.trace(&first.id).is_none());
            assert_eq!(current(&mut second, &mut second_table), second_span.id());
            assert_eq!(
                current(&mut first, &mut ResourceTable::new()),
                component.id()
            );
            drop(second_table);
            assert!(scopes.trace(&second.id).is_none());
            assert!(Arc::ptr_eq(&scopes.release("component").unwrap(), &release));
        });
    }
}
