//! traceproof (9.2): prove outbound `traceparent` injection is host-enforced
//! independently on wash-runtime's P2 and P3 HTTP surfaces.
//!
//! Topology:
//!
//! ```text
//!   controlled parent trace 00-T-S0-01
//!             |
//!             +--> HostHandler::outgoing_request (P2) ----+
//!             |                                            |
//!             +--> HostHandler::outgoing_request_p3 (P3) --+--> capture-only
//!                                                                transport
//!                                                                 |
//!               serve-echo (separate pod) <-- raw GET built only -+
//!                                             from that surface's
//!                                             captured header
//! ```
//!
//! No guest participates. The custom transport only captures what each public
//! host surface hands it; it never injects. A network request is refused unless
//! that surface produced a valid host-injected header. Sending only that
//! captured header to `serve-echo` composes the host-boundary proof with the
//! cross-process proof. P2 and P3 have separate named assertions, so removing
//! either inject cannot hide behind the other surface.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, bail};
use bytes::Bytes;
use clap::Args;
use http_body_util::BodyExt as _;
use opentelemetry::trace::{
    SpanContext, SpanId, TraceContextExt as _, TraceFlags, TraceId, TraceState,
};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use wamn_test_fixtures::runner::fnv1a_64;
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::host::http::{DevRouter, HostHandler as _, HttpServer, OutgoingHandler};
use wash_runtime::host::http_p3::{P3Body, P3RequestErrorFuture, P3SendFuture};
use wasmtime_wasi_http::p2::bindings::http::types::ErrorCode as P2ErrorCode;
use wasmtime_wasi_http::p2::body::HyperOutgoingBody;
use wasmtime_wasi_http::p2::types::{HostFutureIncomingResponse, OutgoingRequestConfig};
use wasmtime_wasi_http::p2::{HttpError, HttpResult};

// ---------------------------------------------------------------------------
// serve-echo: the reflecting upstream (plain HTTP, not wash-served)
// ---------------------------------------------------------------------------

#[derive(Args, Debug)]
pub struct ServeEchoArgs {
    /// Port to listen on.
    #[arg(long, default_value_t = 8091)]
    pub port: u16,
}

/// A tiny HTTP/1.1 server (the `serve-node` hand-rolled pattern) that answers
/// every request 200 with `{"traceparent": <received|null>, "tracestate":
/// <received|null>, "authorization-fnv1a": <hex-digest|null>}`. It reflects
/// exactly the trace headers it was sent — so traceproof can read what each
/// host surface injected — plus a ONE-WAY FNV-1a
/// digest of the `authorization` header, which credproof (5.9) uses as the
/// delivery witness for a vault-resolved credential. A digest (never the raw
/// value) keeps the secret out of the flow's recorded payloads, so the
/// credproof containment scan can be TOTAL — the secret must appear in no
/// recorded row at all.
pub async fn serve_echo(args: ServeEchoArgs) -> anyhow::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", args.port)).await?;
    println!(
        "serve-echo: reflecting trace headers on 0.0.0.0:{}",
        args.port
    );
    loop {
        let (sock, _peer) = listener.accept().await?;
        tokio::spawn(async move {
            if let Err(e) = echo_connection(sock).await {
                tracing::warn!("serve-echo connection error: {e}");
            }
        });
    }
}

async fn echo_connection(sock: TcpStream) -> anyhow::Result<()> {
    sock.set_nodelay(true)?;
    let mut reader = BufReader::new(sock);
    loop {
        let Some(headers) = read_request_head(&mut reader).await? else {
            break; // client closed
        };
        let tp = header_of(&headers, "traceparent");
        let ts = header_of(&headers, "tracestate");
        let auth_digest = header_of(&headers, "authorization")
            .map(|a| format!("{:016x}", fnv1a_64(a.as_bytes())));
        let body = serde_json::json!({
            "traceparent": tp,
            "tracestate": ts,
            "authorization-fnv1a": auth_digest,
        })
        .to_string();
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{}",
            body.len(),
            body
        );
        reader.get_mut().write_all(resp.as_bytes()).await?;
        reader.get_mut().flush().await?;
    }
    Ok(())
}

/// Read request head lines up to the blank line; returns the header lines
/// (request-line included), or `None` at EOF. Bodies are ignored (GET).
async fn read_request_head(
    reader: &mut BufReader<TcpStream>,
) -> anyhow::Result<Option<Vec<String>>> {
    let mut lines = Vec::new();
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(if lines.is_empty() { None } else { Some(lines) });
        }
        let trimmed = line.trim_end_matches(['\r', '\n']).to_string();
        if trimmed.is_empty() {
            return Ok(Some(lines));
        }
        lines.push(trimmed);
    }
}

/// Case-insensitive header lookup over `Name: value` lines.
fn header_of(headers: &[String], name: &str) -> Option<String> {
    for line in headers {
        if let Some((k, v)) = line.split_once(':')
            && k.trim().eq_ignore_ascii_case(name)
        {
            return Some(v.trim().to_string());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// traceproof: the assertion driver
// ---------------------------------------------------------------------------

#[derive(Args, Debug)]
pub struct TraceproofArgs {
    /// Reflecting upstream in a separate process/pod.
    #[arg(long)]
    pub upstream: String,
}

pub async fn run(args: TraceproofArgs) -> anyhow::Result<()> {
    // A unique, valid W3C traceparent we control. `01` = sampled.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let trace_id = format!("{nanos:032x}"); // 32 hex chars
    let trace_id = trace_id[trace_id.len() - 32..].to_string();
    let sent_span = format!("{:016x}", nanos as u64 | 1); // 16 hex, non-zero
    let sent_tp = format!("00-{trace_id}-{sent_span}-01");
    println!("controlled parent traceparent = {sent_tp}");

    let parent = parent_context(&trace_id, &sent_span)?;
    let mut failures = Vec::new();
    for surface in [HttpSurface::P2, HttpSurface::P3] {
        match prove_surface(surface, &args.upstream, &parent, &trace_id, &sent_span).await {
            Ok(()) => println!(
                "PASS [{surface} trace id threads across process boundary without guest help]"
            ),
            Err(error) => {
                eprintln!("{error:#}");
                failures.push(surface);
            }
        }
    }
    if failures.is_empty() {
        println!("traceproof: overall PASS (independent P2 + P3 host-enforced inject)");
        Ok(())
    } else {
        bail!("traceproof: overall FAIL (failed surfaces: {failures:?})")
    }
}

#[derive(Clone, Copy, Debug)]
enum HttpSurface {
    P2,
    P3,
}

impl std::fmt::Display for HttpSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::P2 => f.write_str("P2"),
            Self::P3 => f.write_str("P3"),
        }
    }
}

async fn prove_surface(
    surface: HttpSurface,
    upstream: &str,
    parent: &opentelemetry::Context,
    sent_trace_id: &str,
    sent_span_id: &str,
) -> anyhow::Result<()> {
    let captured = capture_host_traceparent(surface, upstream, parent)
        .await
        .with_context(|| format!("FAIL [{surface} host surface executes]"))?
        .with_context(|| {
            format!("FAIL [{surface} host surface injected traceparent without guest help]")
        })?;
    validate_traceparent(surface, &captured, sent_trace_id, sent_span_id)
        .with_context(|| format!("FAIL [{surface} host surface injected valid traceparent]"))?;

    // The cross-process request is constructed exclusively from this surface's
    // post-HostHandler capture. No fallback to the caller's original header is
    // allowed: absent/invalid injection returns above without touching the net.
    let body = http_get_with_headers(upstream, &[("traceparent".to_string(), captured.clone())])
        .await
        .with_context(|| format!("FAIL [{surface} captured header crosses process boundary]"))?;
    let reflected: serde_json::Value = serde_json::from_str(&body)
        .with_context(|| format!("FAIL [{surface} parse serve-echo body {body:?}]"))?;
    let reflected_tp = reflected
        .get("traceparent")
        .and_then(|value| value.as_str())
        .with_context(|| {
            format!(
                "FAIL [{surface} downstream received captured traceparent]: serve-echo body {body}"
            )
        })?;
    let reflected_trace = w3c_field(reflected_tp, 1);
    if reflected_trace.as_deref() != Some(sent_trace_id) {
        bail!(
            "FAIL [{surface} trace id threads across process boundary]: \
             reflected trace id {reflected_trace:?} != sent {sent_trace_id}"
        );
    }
    if reflected_tp != captured {
        bail!(
            "FAIL [{surface} downstream received exactly the captured host header]: \
             reflected {reflected_tp:?} != captured {captured:?}"
        );
    }
    println!("{surface} captured traceparent = {captured}");
    Ok(())
}

fn validate_traceparent(
    surface: HttpSurface,
    traceparent: &str,
    sent_trace_id: &str,
    sent_span_id: &str,
) -> anyhow::Result<()> {
    let fields: Vec<_> = traceparent.split('-').collect();
    if fields.len() != 4
        || fields[0] != "00"
        || fields[1].len() != 32
        || fields[2].len() != 16
        || fields[3].len() != 2
        || !fields
            .iter()
            .all(|field| field.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        bail!("{surface} produced malformed traceparent {traceparent:?}");
    }
    if fields[1] != sent_trace_id {
        bail!(
            "{surface} injected trace id {} != sent {sent_trace_id}",
            fields[1]
        );
    }
    if fields[2] == sent_span_id {
        bail!("{surface} injected caller span id {sent_span_id} instead of a host child span");
    }
    Ok(())
}

fn parent_context(trace_id: &str, span_id: &str) -> anyhow::Result<opentelemetry::Context> {
    let trace_id = TraceId::from_hex(trace_id).context("parse controlled trace id")?;
    let span_id = SpanId::from_hex(span_id).context("parse controlled span id")?;
    Ok(
        opentelemetry::Context::new().with_remote_span_context(SpanContext::new(
            trace_id,
            span_id,
            TraceFlags::SAMPLED,
            true,
            TraceState::default(),
        )),
    )
}

#[derive(Clone, Default)]
struct CaptureOutgoing {
    traceparent: Arc<Mutex<Option<String>>>,
}

impl CaptureOutgoing {
    fn record<B>(&self, request: &hyper::Request<B>) {
        let value = request
            .headers()
            .get("traceparent")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        *self
            .traceparent
            .lock()
            .expect("traceproof capture mutex must not be poisoned") = value;
    }

    fn captured(&self) -> Option<String> {
        self.traceparent
            .lock()
            .expect("traceproof capture mutex must not be poisoned")
            .clone()
    }
}

impl OutgoingHandler for CaptureOutgoing {
    fn send_request(
        &self,
        _workload_id: &str,
        request: hyper::Request<HyperOutgoingBody>,
        _config: OutgoingRequestConfig,
    ) -> HttpResult<HostFutureIncomingResponse> {
        self.record(&request);
        Err(HttpError::trap(P2ErrorCode::InternalError(Some(
            "traceproof capture transport stops after the P2 host boundary".to_string(),
        ))))
    }

    fn send_request_p3(
        &self,
        _workload_id: &str,
        request: hyper::Request<P3Body>,
        _options: Option<wasmtime_wasi_http::p3::RequestOptions>,
        _fut: P3RequestErrorFuture,
    ) -> P3SendFuture {
        self.record(&request);
        let body: P3Body = http_body_util::Empty::<Bytes>::new()
            .map_err(|never| match never {})
            .boxed_unsync();
        let response = hyper::Response::new(body);
        Box::new(async move {
            let io: P3RequestErrorFuture = Box::new(async { Ok(()) });
            Ok((response, io))
        })
    }
}

async fn capture_host_traceparent(
    surface: HttpSurface,
    upstream: &str,
    parent: &opentelemetry::Context,
) -> anyhow::Result<Option<String>> {
    let capture = CaptureOutgoing::default();
    let server = HttpServer::builder(DevRouter::default(), "127.0.0.1:0".parse()?)
        .outgoing_handler(capture.clone())
        .build()
        .await
        .context("build traceproof host HTTP surface")?;
    let span = tracing::info_span!("traceproof.host_outbound", surface = %surface);
    span.set_parent(parent.clone())
        .context("attach controlled parent to host span")?;
    let _entered = span.enter();
    let allow_any = [AllowedHost::Any];

    match surface {
        HttpSurface::P2 => {
            let request = hyper::Request::builder()
                .uri(upstream)
                .body(HyperOutgoingBody::default())
                .context("build P2 outgoing request")?;
            let config = OutgoingRequestConfig {
                use_tls: false,
                connect_timeout: Duration::from_secs(5),
                first_byte_timeout: Duration::from_secs(5),
                between_bytes_timeout: Duration::from_secs(5),
            };
            let _ = server.outgoing_request("traceproof-p2", request, config, &allow_any);
        }
        HttpSurface::P3 => {
            let body: P3Body = http_body_util::Empty::<Bytes>::new()
                .map_err(|never| match never {})
                .boxed_unsync();
            let request = hyper::Request::builder()
                .uri(upstream)
                .body(body)
                .context("build P3 outgoing request")?;
            let request_error: P3RequestErrorFuture = Box::new(async { Ok(()) });
            let send = server.outgoing_request_p3(
                "traceproof-p3",
                request,
                None,
                request_error,
                &allow_any,
            );
            drop(send);
        }
    }
    Ok(capture.captured())
}

/// Extract field `idx` (0=version,1=trace-id,2=parent-id,3=flags) of a W3C
/// `traceparent`.
fn w3c_field(tp: &str, idx: usize) -> Option<String> {
    tp.split('-').nth(idx).map(|s| s.to_string())
}

/// Hand-rolled HTTP/1.1 GET with extra headers, returning the declared response
/// body. The controlled `serve-echo` peer emits `Content-Length` and may keep
/// the connection alive, so completion is framed by that length rather than
/// waiting for EOF. Fails on a non-HTTP URL, non-2xx status, or unframed body.
async fn http_get_with_headers(url: &str, extra: &[(String, String)]) -> anyhow::Result<String> {
    let uri: hyper::Uri = url.parse().with_context(|| format!("parse URL {url:?}"))?;
    if uri.scheme_str() != Some("http") {
        bail!("traceproof supports only http:// upstreams, got {url:?}");
    }
    let authority = uri
        .authority()
        .with_context(|| format!("URL has no authority: {url:?}"))?;
    let conn_host = authority.host();
    let port = authority.port_u16().unwrap_or(80);
    let path = uri
        .path_and_query()
        .map_or("/", hyper::http::uri::PathAndQuery::as_str);
    let mut stream = TcpStream::connect((conn_host, port))
        .await
        .with_context(|| format!("connect {conn_host}:{port}"))?;
    let mut req = format!("GET {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n");
    for (k, v) in extra {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).await?;
    stream.flush().await?;

    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    if reader.read_line(&mut status).await? == 0 {
        bail!("GET {authority}{path} -> EOF before status");
    }
    if !(status.starts_with("HTTP/1.1 2") || status.starts_with("HTTP/1.0 2")) {
        bail!(
            "GET {authority}{path} -> {}",
            status.trim_end_matches(['\r', '\n'])
        );
    }

    let mut content_length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await? == 0 {
            bail!("GET {authority}{path} -> EOF before response headers completed");
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .context("parse response Content-Length")?,
            );
        }
    }
    let content_length =
        content_length.context("serve-echo response must declare Content-Length")?;
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body).await?;
    String::from_utf8(body).context("serve-echo response body must be UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::trace::TracerProvider as _;
    use tracing_subscriber::layer::SubscriberExt as _;

    fn install_test_tracer() -> (
        opentelemetry_sdk::trace::SdkTracerProvider,
        tracing::subscriber::DefaultGuard,
    ) {
        opentelemetry::global::set_text_map_propagator(
            opentelemetry_sdk::propagation::TraceContextPropagator::new(),
        );
        let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder().build();
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("traceproof-test")));
        let guard = tracing::subscriber::set_default(subscriber);
        (provider, guard)
    }

    async fn captured_for(surface: HttpSurface) -> String {
        const TRACE_ID: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
        const SPAN_ID: &str = "00f067aa0ba902b7";
        let parent = parent_context(TRACE_ID, SPAN_ID).expect("valid controlled parent");
        let captured = capture_host_traceparent(surface, "http://example.com/", &parent)
            .await
            .expect("host surface executes")
            .unwrap_or_else(|| panic!("{surface} host surface must inject traceparent"));
        validate_traceparent(surface, &captured, TRACE_ID, SPAN_ID).unwrap_or_else(|error| {
            panic!("{surface} host surface traceparent invalid: {error:#}")
        });
        captured
    }

    #[tokio::test(flavor = "current_thread")]
    async fn p2_host_surface_injects_traceparent_without_guest_help() {
        let (provider, guard) = install_test_tracer();
        let captured = captured_for(HttpSurface::P2).await;
        assert_eq!(
            w3c_field(&captured, 1).as_deref(),
            Some("4bf92f3577b34da6a3ce929d0e0e4736")
        );
        drop(guard);
        provider
            .shutdown()
            .expect("traceproof test tracer must shut down");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn p3_host_surface_injects_traceparent_without_guest_help() {
        let (provider, guard) = install_test_tracer();
        let captured = captured_for(HttpSurface::P3).await;
        assert_eq!(
            w3c_field(&captured, 1).as_deref(),
            Some("4bf92f3577b34da6a3ce929d0e0e4736")
        );
        drop(guard);
        provider
            .shutdown()
            .expect("traceproof test tracer must shut down");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn http_get_returns_content_length_body_without_waiting_for_close() {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind keep-alive fixture");
        let address = listener.local_addr().expect("fixture address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept fixture request");
            let mut request = [0_u8; 1];
            stream
                .read_exact(&mut request)
                .await
                .expect("read fixture request");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nok",
                )
                .await
                .expect("write fixture response");
            stream.flush().await.expect("flush fixture response");
            tokio::time::sleep(Duration::from_secs(10)).await;
        });

        let body = tokio::time::timeout(
            Duration::from_secs(2),
            http_get_with_headers(&format!("http://{address}/"), &[]),
        )
        .await
        .expect("Content-Length must complete before the peer closes")
        .expect("read fixture response");
        assert_eq!(body, "ok");
        server.abort();
    }

    #[test]
    fn w3c_fields_split_a_traceparent() {
        let tp = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";
        assert_eq!(w3c_field(tp, 0).as_deref(), Some("00"));
        assert_eq!(
            w3c_field(tp, 1).as_deref(),
            Some("0af7651916cd43dd8448eb211c80319c")
        );
        assert_eq!(w3c_field(tp, 2).as_deref(), Some("b7ad6b7169203331"));
        assert_eq!(w3c_field(tp, 3).as_deref(), Some("01"));
    }

    #[test]
    fn header_of_is_case_insensitive() {
        let h = vec![
            "GET / HTTP/1.1".to_string(),
            "Host: relay".to_string(),
            "TraceParent: 00-abc-def-01".to_string(),
        ];
        assert_eq!(
            header_of(&h, "traceparent").as_deref(),
            Some("00-abc-def-01")
        );
        assert_eq!(header_of(&h, "x-missing"), None);
    }
}
