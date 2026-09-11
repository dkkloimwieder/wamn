//! Native tasks retain the caller's trace through nested guest and host calls.

use opentelemetry::trace::{
    SpanContext, SpanId, TraceContextExt as _, TraceFlags, TraceId, TraceState, TracerProvider as _,
};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use tracing_subscriber::layer::SubscriberExt as _;
use wamn_runtime::plugins::flow_http_routing::AuthenticatedCaller;

use super::{CHILD, Fixture, ROOT};

fn attribute<'a>(span: &'a SpanData, key: &str) -> Option<&'a str> {
    span.attributes.iter().find_map(|attribute| {
        if attribute.key.as_str() == key
            && let opentelemetry::Value::String(value) = &attribute.value
        {
            Some(value.as_str())
        } else {
            None
        }
    })
}

pub(super) struct TraceProof {
    provider: SdkTracerProvider,
    exporter: InMemorySpanExporter,
    incoming: SpanContext,
    pub(super) span: tracing::Span,
    pub(super) dispatcher: tracing::Dispatch,
}

impl TraceProof {
    pub(super) fn new(fixture: &Fixture, caller: &AuthenticatedCaller) -> Self {
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("native-call-proof")));
        let dispatcher = tracing::Dispatch::new(subscriber);
        let incoming = SpanContext::new(
            TraceId::from_bytes([0x41; 16]),
            SpanId::from_bytes([0x17; 8]),
            TraceFlags::SAMPLED,
            true,
            TraceState::default(),
        );
        let root = tracing::dispatcher::with_default(&dispatcher, || {
            tracing::info_span!(
                target: "wamn::router",
                "wamn.component.invoke",
                wamn.operation = ROOT,
                wamn.component_digest = %fixture.root.component_digest,
                wamn.caller_principal_id = caller.principal_id(),
                wamn.caller_credential_kind = "session",
            )
        });
        root.set_parent(opentelemetry::Context::new().with_remote_span_context(incoming.clone()))
            .expect("the root invocation accepts the incoming trace");
        Self {
            provider,
            exporter,
            incoming,
            span: root,
            dispatcher,
        }
    }

    pub(super) fn assert_parentage(self, fixture: &Fixture, caller: &AuthenticatedCaller) {
        let Self {
            provider,
            exporter,
            incoming,
            span,
            dispatcher,
        } = self;
        drop(span);
        drop(dispatcher);
        provider.force_flush().expect("flush native call spans");
        let spans = exporter.get_finished_spans().expect("exported trace");
        for span in spans
            .iter()
            .filter(|span| {
                matches!(
                    span.name.as_ref(),
                    "wamn.component.invoke" | "proof.host.observe"
                )
            })
            .take(8)
        {
            println!(
                "authenticated-native-span name={} operation={} trace={} span={} parent={}",
                span.name,
                attribute(span, "wamn.operation").unwrap_or("missing"),
                span.span_context.trace_id(),
                span.span_context.span_id(),
                span.parent_span_id,
            );
        }
        let invocations: Vec<_> = spans
            .iter()
            .filter(|span| span.name == "wamn.component.invoke")
            .collect();
        assert_eq!(
            invocations.len(),
            2,
            "exactly one root and one child invocation"
        );
        let root = invocations
            .iter()
            .find(|span| attribute(span, "wamn.operation") == Some(ROOT))
            .expect("the original root span");
        let child = invocations
            .iter()
            .find(|span| attribute(span, "wamn.operation") == Some(CHILD))
            .expect("the nested child span");
        assert_eq!(root.span_context.trace_id(), incoming.trace_id());
        assert_eq!(root.parent_span_id, incoming.span_id());
        assert_eq!(child.span_context.trace_id(), incoming.trace_id());
        assert_eq!(child.parent_span_id, root.span_context.span_id());
        for invocation in [root, child] {
            assert_eq!(
                attribute(invocation, "wamn.caller_principal_id"),
                Some(caller.principal_id())
            );
            assert_eq!(
                attribute(invocation, "wamn.caller_credential_kind"),
                Some("session")
            );
        }
        let effects: Vec<_> = spans
            .iter()
            .filter(|span| span.name == "proof.host.observe")
            .collect();
        assert_eq!(
            effects.len(),
            2,
            "each executed guest issues its host observation effect"
        );
        for invocation in [root, child] {
            let operation =
                attribute(invocation, "wamn.operation").expect("host operation identity");
            let fact = fixture
                .workload
                .facts_by_component_id
                .values()
                .find(|fact| fact.operations.contains_key(operation))
                .expect("admitted operation owner");
            assert_eq!(
                attribute(invocation, "wamn.component_digest"),
                Some(fact.component_digest.as_str())
            );
            let effect = effects
                .iter()
                .find(|span| attribute(span, "wamn.operation") == Some(operation))
                .expect("the guest performed its host effect");
            assert_eq!(effect.span_context.trace_id(), incoming.trace_id());
            assert_eq!(effect.parent_span_id, invocation.span_context.span_id());
            assert_eq!(
                attribute(effect, "wamn.component_digest"),
                Some(fact.component_digest.as_str())
            );
        }
        println!("authenticated-native-trace result=pass invocations=2 host_observations=2");
    }
}
