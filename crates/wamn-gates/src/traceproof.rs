//! traceproof (9.2): the DEPLOYED cross-pod proof that outbound `traceparent`
//! injection is HOST-ENFORCED. The in-proc bench path (`tracebench`, 9.1)
//! bypasses wash-runtime's HTTP server, so it cannot exercise the outbound
//! send path where the fork stamps the trace context — this gate must run
//! against real washlet workloads.
//!
//! Topology:
//!
//! ```text
//!   traceproof (this) --GET, traceparent=00-T-S0-01--> trace-relay (wash pod A)
//!        trace-relay makes a BARE outbound GET (no trace header) ----------+
//!                                                                          v
//!                                              serve-echo (plain pod B) reflects
//!                                              the traceparent IT received in JSON
//!        <-- relay returns serve-echo's body verbatim -------------------- +
//! ```
//!
//! wash extracts the inbound `traceparent` (continuing trace `T`) and the fork
//! stamps `T` onto the relay's bare outbound call. The relay never sets a trace
//! header, so a `traceparent` arriving at `serve-echo` can ONLY come from the
//! host inject. Asserts (each a NAMED failure the inject mutation flips):
//!   1. the downstream received a `traceparent` at all;
//!   2. its trace id equals the one we sent (the trace threads the boundary);
//!   3. its span id differs from the one we sent (the host minted a child
//!      client span — not a blind header copy).

use anyhow::{Context, bail};
use clap::Args;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

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
/// exactly the trace headers it was sent — so traceproof can read what the
/// host injected onto the relay's outbound call — plus a ONE-WAY FNV-1a
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
    use tokio::io::AsyncBufReadExt;
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

/// FNV-1a 64 (the house inline digest): the one-way witness serve-echo
/// reflects for a received `authorization` header — proves the exact value
/// arrived without echoing the secret back into recorded payloads.
pub(crate) fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
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
    /// Base URL of the trace-relay workload's Service, e.g. `http://trace-relay:80`.
    #[arg(long)]
    pub relay_url: String,
    /// Host header the relay is routed under (wash `DynamicRouter` key).
    #[arg(long)]
    pub relay_host: String,
    /// Upstream the relay should call (passed as `x-relay-upstream`); the
    /// relay's compiled default is used when omitted.
    #[arg(long)]
    pub upstream: Option<String>,
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

    let mut headers = vec![("traceparent".to_string(), sent_tp.clone())];
    if let Some(up) = &args.upstream {
        headers.push(("x-relay-upstream".to_string(), up.clone()));
    }

    let body = http_get_with_headers(&args.relay_url, "/", &args.relay_host, &headers)
        .await
        .context("GET the trace-relay")?;
    let reflected: serde_json::Value =
        serde_json::from_str(&body).with_context(|| format!("parse relay body: {body:?}"))?;
    let downstream_tp = reflected.get("traceparent").and_then(|v| v.as_str());

    let mut pass = true;
    let mut fail = |name: &str, detail: String| {
        pass = false;
        eprintln!("FAIL [{name}]: {detail}");
    };

    // 1. The downstream received a traceparent at all — the load-bearing claim;
    //    with the inject disabled the relay's bare outbound carries none.
    let Some(down) = downstream_tp else {
        fail(
            "downstream received traceparent",
            format!("serve-echo saw no traceparent (relay body: {body})"),
        );
        bail_summary(pass);
        return Ok(());
    };
    println!("sent      traceparent = {sent_tp}");
    println!("reflected traceparent = {down}");

    // 2. Its trace id threads the pod boundary (equals what we sent).
    let down_trace = w3c_field(down, 1);
    if down_trace.as_deref() != Some(trace_id.as_str()) {
        fail(
            "trace id threads across the pod boundary",
            format!("downstream trace id {down_trace:?} != sent {trace_id}"),
        );
    }

    // 3. The host minted a child client span (span id differs from ours), i.e.
    //    it injected the outbound client span's context, not a blind copy.
    let down_span = w3c_field(down, 2);
    if down_span.as_deref() == Some(sent_span.as_str()) {
        fail(
            "host minted a child span, not a blind copy",
            format!("downstream span id == sent span id {sent_span}"),
        );
    }

    bail_summary(pass);
    Ok(())
}

fn bail_summary(pass: bool) {
    if pass {
        println!("traceproof: overall PASS (host-enforced cross-pod traceparent inject)");
    } else {
        // Non-zero exit so the Job/CI marks failure.
        eprintln!("traceproof: overall FAIL");
        std::process::exit(1);
    }
}

/// Extract field `idx` (0=version,1=trace-id,2=parent-id,3=flags) of a W3C
/// `traceparent`.
fn w3c_field(tp: &str, idx: usize) -> Option<String> {
    tp.split('-').nth(idx).map(|s| s.to_string())
}

/// Hand-rolled HTTP/1.1 GET with an explicit Host header and extra headers,
/// returning the (dechunked) response body. Fails on a non-2xx status.
async fn http_get_with_headers(
    base: &str,
    path: &str,
    host: &str,
    extra: &[(String, String)],
) -> anyhow::Result<String> {
    let host_port = base.strip_prefix("http://").unwrap_or(base);
    let (conn_host, port) = match host_port.split_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>().unwrap_or(80)),
        None => (host_port.to_string(), 80),
    };
    let mut stream = TcpStream::connect((conn_host.as_str(), port))
        .await
        .with_context(|| format!("connect {conn_host}:{port}"))?;
    let mut req = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    for (k, v) in extra {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).await?;
    stream.flush().await?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await?;
    let text = String::from_utf8_lossy(&raw);
    if !(text.starts_with("HTTP/1.1 2") || text.starts_with("HTTP/1.0 2")) {
        bail!(
            "GET {host}{path} -> {}",
            text.lines().next().unwrap_or("<none>")
        );
    }
    let (head, body) = text
        .split_once("\r\n\r\n")
        .map(|(h, b)| (h.to_string(), b.to_string()))
        .unwrap_or_default();
    if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        Ok(dechunk(&body))
    } else {
        Ok(body)
    }
}

fn dechunk(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some((size_line, after)) = rest.split_once("\r\n") {
        let size = usize::from_str_radix(size_line.trim().split(';').next().unwrap_or("0"), 16)
            .unwrap_or(0);
        if size == 0 {
            break;
        }
        if after.len() < size {
            out.push_str(after);
            break;
        }
        out.push_str(&after[..size]);
        rest = after.get(size + 2..).unwrap_or("");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
