//! traceproof (9.2): prove outbound `traceparent` injection is host-enforced on
//! `wamn:connection/http`, the surface that carries production egress.
//!
//! Topology:
//!
//! ```text
//!   controlled parent trace 00-T-S0-01
//!             |
//!             +--> wamn.connection_http effect span
//!                             |
//!                             +--> inject_trace_context fills the outbound
//!                                  header map (the shipped injector)
//!                                                 |
//!               serve-echo (separate pod) <-- raw GET built only -----+
//!                                             from that injected header
//! ```
//!
//! No guest participates. The header is produced by the production injector
//! [`inject_trace_context`] — the same function [`ConnectionHttp::send`] reaches
//! through `outbound_headers` — never by this proof, so an injector that stops
//! injecting fails here exactly as it would in production. A network request is
//! refused unless that injector produced a valid header; sending only that
//! header to `serve-echo` composes the host-boundary proof with the
//! cross-process proof.
//!
//! RE-AIMED 2026-08-26 (`wamn-k9ea`). This gate previously drove wash-runtime's
//! P2 and P3 `wasi:http` host surfaces. That injection WAS fork patch `g2br.4`,
//! which `docs/architecture/native-alignment-ledger.md` records as **Dropped**
//! at the v2.8.0 sync: WAMN's real outbound-effect path is
//! `wamn:connection/http`, which injects the active span context itself, and the
//! `wasi:http` egress surface has no WAMN production call site. The two
//! host-surface arms were that drop's untaken test tail. The gate keeps a
//! cross-process subject rather than retiring, because the surviving unit proofs
//! on the live path are all in-process.
//!
//! [`ConnectionHttp::send`]: wamn_runtime::plugins::connection_http::ConnectionHttp

use anyhow::{Context, bail};
use clap::Args;
use opentelemetry::trace::{
    SpanContext, SpanId, TraceContextExt as _, TraceFlags, TraceId, TraceState,
};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use wamn_runtime::plugins::connection_http::inject_trace_context;

/// The egress surface this gate proves, named once so every assertion message
/// and the overall verdict cannot drift apart.
const SURFACE: &str = "wamn:connection/http";

// ---------------------------------------------------------------------------
// serve-echo: the reflecting upstream (plain HTTP, not wash-served)
// ---------------------------------------------------------------------------

#[derive(Args, Debug)]
pub struct ServeEchoArgs {
    /// Port to listen on.
    #[arg(long, default_value_t = 8091)]
    pub port: u16,
}

/// A tiny HTTP/1.1 server that answers
/// every request 200 with `{"traceparent": <received|null>, "tracestate":
/// <received|null>}`. It reflects exactly the trace headers it was sent, so
/// traceproof can read what the effect surface injected.
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
        let body = serde_json::json!({
            "traceparent": tp,
            "tracestate": ts,
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
    match prove_effect_surface(&args.upstream, &parent, &trace_id, &sent_span).await {
        Ok(()) => {
            println!(
                "PASS [{SURFACE} trace id threads across process boundary without guest help]"
            );
            println!(
                "traceproof: overall PASS (host-enforced effect-span inject on the production \
                 egress surface)"
            );
            Ok(())
        }
        Err(error) => {
            eprintln!("{error:#}");
            bail!("traceproof: overall FAIL ({SURFACE})")
        }
    }
}

async fn prove_effect_surface(
    upstream: &str,
    parent: &opentelemetry::Context,
    sent_trace_id: &str,
    sent_span_id: &str,
) -> anyhow::Result<()> {
    let captured = capture_effect_traceparent(parent)
        .with_context(|| format!("FAIL [{SURFACE} effect span executes]"))?
        .with_context(|| {
            format!("FAIL [{SURFACE} effect span injected traceparent without guest help]")
        })?;
    validate_traceparent(&captured, sent_trace_id, sent_span_id)
        .with_context(|| format!("FAIL [{SURFACE} effect span injected valid traceparent]"))?;

    // The cross-process request is constructed exclusively from what the
    // production injector wrote. No fallback to the caller's original header is
    // allowed: absent/invalid injection returns above without touching the net.
    let body = http_get_with_headers(upstream, &[("traceparent".to_string(), captured.clone())])
        .await
        .with_context(|| format!("FAIL [{SURFACE} injected header crosses process boundary]"))?;
    let reflected: serde_json::Value = serde_json::from_str(&body)
        .with_context(|| format!("FAIL [{SURFACE} parse serve-echo body {body:?}]"))?;
    let reflected_tp = reflected
        .get("traceparent")
        .and_then(|value| value.as_str())
        .with_context(|| {
            format!(
                "FAIL [{SURFACE} downstream received injected traceparent]: serve-echo body {body}"
            )
        })?;
    let reflected_trace = w3c_field(reflected_tp, 1);
    if reflected_trace.as_deref() != Some(sent_trace_id) {
        bail!(
            "FAIL [{SURFACE} trace id threads across process boundary]: \
             reflected trace id {reflected_trace:?} != sent {sent_trace_id}"
        );
    }
    if reflected_tp != captured {
        bail!(
            "FAIL [{SURFACE} downstream received exactly the injected host header]: \
             reflected {reflected_tp:?} != captured {captured:?}"
        );
    }
    println!("{SURFACE} injected traceparent = {captured}");
    Ok(())
}

fn validate_traceparent(
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
        bail!("{SURFACE} produced malformed traceparent {traceparent:?}");
    }
    if fields[1] != sent_trace_id {
        bail!(
            "{SURFACE} injected trace id {} != sent {sent_trace_id}",
            fields[1]
        );
    }
    if fields[2] == sent_span_id {
        bail!("{SURFACE} injected caller span id {sent_span_id} instead of a host child span");
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

/// The `traceparent` the production injector puts on an outbound effect
/// request, under a `wamn.connection_http` span parented to `parent`.
///
/// Nothing here writes a header. [`inject_trace_context`] is the shipped
/// function `ConnectionHttp::outbound_headers` calls, so an injector that stops
/// injecting returns `None` here exactly as it would in production, and the
/// caller refuses to touch the network. Reading the header back out of the map
/// is how this proof observes what leaves the host.
fn capture_effect_traceparent(
    parent: &opentelemetry::Context,
) -> anyhow::Result<Option<String>> {
    let span = tracing::info_span!("wamn.connection_http");
    span.set_parent(parent.clone())
        .context("attach controlled parent to the effect span")?;
    let _entered = span.enter();

    let mut headers = reqwest::header::HeaderMap::new();
    inject_trace_context(&mut headers);
    Ok(headers
        .get("traceparent")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string))
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
    use std::time::Duration;

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

    /// The in-process half of the gate, named so the cross-process half is not
    /// the only thing standing between a silent injector and a green run.
    ///
    /// The property is a CHILD, not an echo: the controlled parent's trace id
    /// must survive onto the wire under a span id that is NOT the caller's.
    /// That is what lets a downstream service parent to the effect rather than
    /// to whatever invoked the host. Driving the shipped
    /// [`inject_trace_context`] rather than a copy is what makes it a proof of
    /// production behaviour.
    #[test]
    fn the_effect_span_injects_a_child_of_the_controlled_parent() {
        const TRACE_ID: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
        const SPAN_ID: &str = "00f067aa0ba902b7";

        let (provider, guard) = install_test_tracer();
        let parent = parent_context(TRACE_ID, SPAN_ID).expect("valid controlled parent");
        let captured = capture_effect_traceparent(&parent)
            .expect("the effect span executes")
            .expect("the production injector must inject traceparent");
        validate_traceparent(&captured, TRACE_ID, SPAN_ID)
            .unwrap_or_else(|error| panic!("{SURFACE} traceparent invalid: {error:#}"));
        assert_eq!(
            w3c_field(&captured, 1).as_deref(),
            Some(TRACE_ID),
            "the controlled parent's trace id must reach the wire"
        );
        assert_ne!(
            w3c_field(&captured, 2).as_deref(),
            Some(SPAN_ID),
            "the effect span, not its caller, must be the outbound parent"
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
