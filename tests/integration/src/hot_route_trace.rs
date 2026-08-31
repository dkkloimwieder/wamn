//! wamn-0h0g.11.8: trace propagation on the hot route, ingress to outbound socket.
//!
//! One incoming `traceparent` reaches the router driver, one component
//! invocation performs an outbound HTTP effect through the trusted connection
//! adapter, and A REAL SOCKET RECEIVES A `traceparent` CARRYING THE SAME TRACE
//! ID. That socket assertion is the load-bearing one — it witnesses
//! `wamn-0h0g.12.6`'s fix at the wire, not only in exporter memory.
//!
//! Entry point: `RouterDriver::execute`. `RouterDeliveryBridge::deliver` is
//! private to `wamn-execution-host` and reachable only by a guest importing
//! `wamn:router-delivery`, and `execute_candidate` needs a frozen gate report
//! and binding world on top of everything `execute` needs — so `execute` is the
//! cheapest entry that still exercises the whole chain from
//! `remote_trace_context` to `inject_trace_context`.
//!
//! Hot route only. The queue leg is a ratified host-scoped re-root
//! (`queue_delivery_span_is_an_explicit_host_scoped_root`) and the stream leg
//! sends no trace at all, so "all under the incoming trace" is true of this leg
//! and of no other.
//!
//! Registers no gate: a plain `cargo test`, no orchestrator subcommand, no
//! `deploy/gates` job, no `architecture/gate-registry.json` entry, no decision
//! id.

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_sdk::propagation::TraceContextPropagator;
    use opentelemetry_sdk::trace::{
        InMemorySpanExporter, InMemorySpanExporterBuilder, SdkTracerProvider, SpanData,
    };
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tracing_subscriber::layer::SubscriberExt as _;
    use wamn_execution_host::{RouterDriverRequest, WiringResolution};

    use crate::trusted_http_route::{
        self, ENVIRONMENT, PACKAGE, RouteOptions, TENANT, WIRING_ID, WIRING_VERSION,
    };

    const TRACE_ID: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
    const INCOMING_TRACEPARENT: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
    const UPSTREAM_PATH: &str = "/ingest";

    /// The in-tree pattern (`crates/execution/host/src/router_driver.rs`,
    /// `crates/platform/runtime/src/plugins/connection_http.rs`,
    /// `services/executor/src/lib.rs`): a real OTel layer, because
    /// `inject_trace_context` has NO no-OTel fallback — without the layer the
    /// host injects nothing and the wire assertion would pass or fail for the
    /// wrong reason.
    struct TraceHarness {
        exporter: InMemorySpanExporter,
        provider: SdkTracerProvider,
        _guard: tracing::subscriber::DefaultGuard,
    }

    impl TraceHarness {
        fn install() -> Self {
            opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
            let exporter = InMemorySpanExporterBuilder::new().build();
            let provider = SdkTracerProvider::builder()
                .with_simple_exporter(exporter.clone())
                .build();
            let subscriber = tracing_subscriber::registry().with(
                tracing_opentelemetry::layer().with_tracer(provider.tracer("hot-route-trace")),
            );
            let guard = tracing::subscriber::set_default(subscriber);
            Self {
                exporter,
                provider,
                _guard: guard,
            }
        }

        fn spans(&self) -> Vec<SpanData> {
            self.provider.force_flush().expect("test spans must flush");
            self.exporter
                .get_finished_spans()
                .expect("test span exporter must remain readable")
        }
    }

    fn span_named<'a>(spans: &'a [SpanData], name: &str) -> &'a SpanData {
        spans
            .iter()
            .find(|span| span.name == name)
            .unwrap_or_else(|| {
                let exported: Vec<_> = spans.iter().map(|span| span.name.as_ref()).collect();
                panic!("span {name:?} must be exported; got {exported:?}")
            })
    }

    /// One real upstream origin. It answers exactly one request and hands back
    /// the header block it actually received off the wire.
    async fn upstream_origin() -> (u16, tokio::task::JoinHandle<Vec<(String, String)>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the upstream origin");
        let port = listener
            .local_addr()
            .expect("upstream has an address")
            .port();
        let served = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("the effect connects");
            let mut received = Vec::new();
            let mut byte = [0_u8; 1];
            while !received.ends_with(b"\r\n\r\n") {
                let read = socket.read(&mut byte).await.expect("read the request head");
                assert_ne!(read, 0, "the effect closed before finishing its head");
                received.push(byte[0]);
            }
            let head = String::from_utf8(received).expect("the request head is utf-8");
            let headers: Vec<(String, String)> = head
                .lines()
                .skip(1)
                .filter_map(|line| line.split_once(':'))
                .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
                .collect();
            let length: usize = headers
                .iter()
                .find(|(name, _)| name == "content-length")
                .map_or(0, |(_, value)| value.parse().expect("a numeric length"));
            let mut body = vec![0_u8; length];
            socket
                .read_exact(&mut body)
                .await
                .expect("read the request body");
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\
                      connection: close\r\n\r\n{}",
                )
                .await
                .expect("answer the effect");
            socket.flush().await.expect("flush the answer");
            headers
        });
        (port, served)
    }

    fn required(key: &str) -> Option<String> {
        std::env::var(key).ok().filter(|value| !value.is_empty())
    }

    /// A `traceparent` is `00-<trace-id>-<span-id>-<flags>`.
    fn field_of(traceparent: &str, index: usize) -> &str {
        traceparent
            .split('-')
            .nth(index)
            .unwrap_or_else(|| panic!("{traceparent:?} is not a W3C traceparent"))
    }

    fn trace_id_of(traceparent: &str) -> &str {
        field_of(traceparent, 1)
    }

    /// Field 2 is the sender's own span — the span everything downstream
    /// parents to.
    fn parent_span_of(traceparent: &str) -> &str {
        field_of(traceparent, 2)
    }

    /// THE PROOF. Everything before the assertions is fixture.
    ///
    /// The downstream parent is `wamn.component.invoke`, NOT
    /// `wamn.connection_http`: `inject_trace_context` is only-if-absent and the
    /// guest pushes its node-context header first. Same trace id, one level up —
    /// already flagged in `wamn-0h0g.12.6`'s close, and not a defect. So the
    /// wire is never asserted against the effect span.
    #[tokio::test]
    #[ignore = "requires WAMN_HOTROUTE_PG_URL, WAMN_HOTROUTE_ARTIFACT_BASE and \
                WAMN_HOTROUTE_COMPONENT_WASM"]
    async fn an_incoming_traceparent_reaches_the_outbound_socket_under_the_same_trace() {
        let database_url =
            required("WAMN_HOTROUTE_PG_URL").expect("set WAMN_HOTROUTE_PG_URL to a throwaway db");
        let artifact_base = required("WAMN_HOTROUTE_ARTIFACT_BASE")
            .expect("set WAMN_HOTROUTE_ARTIFACT_BASE to a throwaway <registry>/<repository>");
        let component_wasm = required("WAMN_HOTROUTE_COMPONENT_WASM")
            .expect("set WAMN_HOTROUTE_COMPONENT_WASM to the built http_request.wasm");

        let harness = TraceHarness::install();
        let (port, served) = upstream_origin().await;

        let route = trusted_http_route::build(&RouteOptions {
            database_url,
            artifact_base,
            component_wasm: PathBuf::from(component_wasm),
            upstream_base_url: format!("http://127.0.0.1:{port}"),
            path_and_query: UPSTREAM_PATH.to_owned(),
        })
        .await
        .expect("the trusted HTTP route builds");

        let delivery = route
            .driver
            .execute(RouterDriverRequest {
                tenant_id: TENANT.to_owned(),
                package_id: PACKAGE.to_owned(),
                environment: ENVIRONMENT.to_owned(),
                wiring_id: WIRING_ID.to_owned(),
                wiring_version: WIRING_VERSION,
                delivery_id: "hot-route-delivery-1".to_owned(),
                payload: serde_json::json!({"id": 1}),
                caller_attached: true,
                resolution: WiringResolution::Active,
                role: None,
                user_id: None,
                traceparent: Some(INCOMING_TRACEPARENT.to_owned()),
                tracestate: None,
            })
            .await
            .expect("the hot route delivers");

        let headers = tokio::time::timeout(std::time::Duration::from_secs(10), served)
            .await
            .expect("the upstream origin answered")
            .expect("the upstream origin task did not panic");

        // ---- THE LOAD-BEARING ASSERTION: the wire, not the exporter. ----
        let (_, wire_traceparent) = headers
            .iter()
            .find(|(name, _)| name == "traceparent")
            .unwrap_or_else(|| {
                let seen: Vec<_> = headers.iter().map(|(name, _)| name.as_str()).collect();
                panic!(
                    "the outbound request carried no traceparent; headers {seen:?}; \
                     delivery outcome {:?}",
                    delivery.outcome
                )
            });
        assert_eq!(
            trace_id_of(wire_traceparent),
            TRACE_ID,
            "a real socket must receive the INCOMING trace id, not a fresh one; \
             delivery outcome {:?}",
            delivery.outcome
        );

        // ---- Corroboration in exporter memory. ----
        let spans = harness.spans();
        for name in ["wamn.component.invoke", "wamn.connection_http"] {
            assert_eq!(
                span_named(&spans, name).span_context.trace_id().to_string(),
                TRACE_ID,
                "{name} must run under the incoming trace"
            );
        }

        // The wire's parent is the HOST'S component span, not the ingress
        // header the host was called with. Forwarding that header verbatim is
        // what `wamn-0h0g.12.6` fixed, and it preserves the trace id — so the
        // trace-id assertion above cannot see that regression and this one is
        // the assertion that pins it. It is deliberately NOT
        // `wamn.connection_http`: injection is only-if-absent and the guest
        // pushes its node-context header first, so the effect span is one level
        // below the wire's parent.
        assert_eq!(
            parent_span_of(wire_traceparent),
            span_named(&spans, "wamn.component.invoke")
                .span_context
                .span_id()
                .to_string(),
            "the outbound request must parent to the host's component span, \
             not to whatever called the host"
        );
    }

    /// The exporter is not the wire. Kept as a unit-cheap guard on the parser
    /// the wire assertion depends on.
    #[test]
    fn a_traceparent_yields_its_trace_id_field() {
        assert_eq!(trace_id_of(INCOMING_TRACEPARENT), TRACE_ID);
    }
}
