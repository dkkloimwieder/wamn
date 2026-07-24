//! `erp-sim` — the POC-F4 ERP callback simulator (wamn-lxk).
//!
//! A tiny hand-rolled HTTP/1.1 server (the `serve-echo` pattern, extended to
//! read POST bodies) that models an ERP endpoint receiving disposition
//! callbacks with an idempotency key. Its whole job is to make the F4 `429 +
//! Retry-After` throttle path REAL and to be the exactly-once witness:
//!
//!   * for the first `--fail-first-n` requests carrying a given `Idempotency-Key`
//!     it answers `429` with `Retry-After: <--retry-after-secs>` (integer
//!     seconds — the only shape `wamn-nodes` http parses), forcing the runner to
//!     PARK the run for the backoff;
//!   * the NEXT request under that key is the FIRST effective delivery → `202`;
//!   * any FURTHER request under the SAME key (a duplicate the platform must not
//!     produce) is an idempotent replay → `202` but records NO new delivery.
//!
//! It records per-key counts (`requests` / `rejected_429` / `delivered`), so a
//! caller can assert `delivered == 1` per key (exactly-one-effective-callback)
//! and `rejected_429 == K` (the throttle actually engaged) — reachable via the
//! in-process [`ErpAudit`] handle (f4proof, local) OR a `GET /audit` endpoint
//! (the in-cluster Job). Requests WITHOUT an idempotency key share one global
//! counter (the `--fail-first-n` "globally" mode).
//!
//! Distinct from `serve-echo` (which credproof/f3proof/traceproof depend on for
//! its always-200 reflect) by construction: this one 429s on demand and is a
//! SEPARATE Command, so no existing consumer regresses.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use clap::Args;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

#[derive(Args, Debug)]
pub struct ErpSimArgs {
    /// Port to listen on.
    #[arg(long, default_value_t = 8092)]
    pub port: u16,

    /// How many requests PER idempotency key (or globally, for keyless
    /// requests) are answered 429 before the callback is accepted.
    #[arg(long, default_value_t = 1)]
    pub fail_first_n: u32,

    /// The `Retry-After` value (integer seconds) sent with each 429.
    #[arg(long, default_value_t = 2)]
    pub retry_after_secs: u64,
}

/// One idempotency key's ledger.
#[derive(Debug, Clone, Default)]
pub struct KeyRecord {
    /// Total requests seen under this key (429s + successes).
    pub requests: u64,
    /// How many of those were answered 429 (the throttle horizon).
    pub rejected_429: u64,
    /// Effective deliveries (a 202 that was the FIRST accept for this key).
    /// Correct behavior is exactly 1 — a duplicate replay never bumps it.
    pub delivered: u64,
}

#[derive(Debug, Default)]
struct AuditState {
    fail_first_n: u32,
    retry_after_secs: u64,
    keys: HashMap<String, KeyRecord>,
    /// The keyless global counter (requests with no `Idempotency-Key`).
    no_key: KeyRecord,
}

/// A shared, cloneable handle to the simulator's ledger — the in-process audit
/// surface the gate reads (and the server mutates).
#[derive(Clone)]
pub struct ErpAudit {
    inner: Arc<Mutex<AuditState>>,
}

impl ErpAudit {
    pub fn new(fail_first_n: u32, retry_after_secs: u64) -> ErpAudit {
        ErpAudit {
            inner: Arc::new(Mutex::new(AuditState {
                fail_first_n,
                retry_after_secs,
                keys: HashMap::new(),
                no_key: KeyRecord::default(),
            })),
        }
    }

    /// Record a request under `key` (None = keyless) and decide its response.
    /// Returns `(status, retry_after_secs?)`.
    fn record(&self, key: Option<&str>) -> (u16, Option<u64>) {
        let mut st = self.inner.lock().expect("erp-sim audit poisoned");
        let fail_first_n = st.fail_first_n;
        let retry_after = st.retry_after_secs;
        let rec = match key {
            Some(k) => st.keys.entry(k.to_string()).or_default(),
            None => &mut st.no_key,
        };
        rec.requests += 1;
        if rec.requests <= u64::from(fail_first_n) {
            rec.rejected_429 += 1;
            (429, Some(retry_after))
        } else {
            // The FIRST accept for this key is the one effective delivery; any
            // further accept is an idempotent replay (no new side effect).
            if rec.delivered == 0 {
                rec.delivered += 1;
            }
            (202, None)
        }
    }

    /// The ledger for `key` (a zeroed record if the key was never seen).
    pub fn key(&self, key: &str) -> KeyRecord {
        self.inner
            .lock()
            .expect("erp-sim audit poisoned")
            .keys
            .get(key)
            .cloned()
            .unwrap_or_default()
    }

    /// Total effective deliveries across every key (keyless included).
    pub fn total_deliveries(&self) -> u64 {
        let st = self.inner.lock().expect("erp-sim audit poisoned");
        st.keys.values().map(|r| r.delivered).sum::<u64>() + st.no_key.delivered
    }

    /// The number of distinct idempotency keys observed.
    pub fn distinct_keys(&self) -> usize {
        self.inner
            .lock()
            .expect("erp-sim audit poisoned")
            .keys
            .len()
    }

    /// A JSON snapshot for the `GET /audit` endpoint.
    fn snapshot(&self) -> serde_json::Value {
        let st = self.inner.lock().expect("erp-sim audit poisoned");
        let keys: serde_json::Map<String, serde_json::Value> = st
            .keys
            .iter()
            .map(|(k, r)| {
                (
                    k.clone(),
                    serde_json::json!({
                        "requests": r.requests,
                        "rejected_429": r.rejected_429,
                        "delivered": r.delivered,
                    }),
                )
            })
            .collect();
        serde_json::json!({
            "fail_first_n": st.fail_first_n,
            "retry_after_secs": st.retry_after_secs,
            "distinct_keys": st.keys.len(),
            "total_deliveries": st.keys.values().map(|r| r.delivered).sum::<u64>() + st.no_key.delivered,
            "keys": keys,
        })
    }
}

/// Serve the simulator on `port` against the shared `audit` ledger. Runs until
/// the future is dropped (the gate select!'s it, or the CLI runs it forever).
pub async fn serve(audit: ErpAudit, port: u16) -> anyhow::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", port))
        .await
        .with_context(|| format!("erp-sim bind 0.0.0.0:{port}"))?;
    println!("erp-sim: ERP callback simulator on 0.0.0.0:{port}");
    loop {
        let (sock, _peer) = listener.accept().await?;
        let audit = audit.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(sock, audit).await {
                tracing::warn!("erp-sim connection error: {e}");
            }
        });
    }
}

/// One keep-alive connection: read a request (head + body), answer it, repeat
/// until the client half-closes.
async fn handle_connection(sock: TcpStream, audit: ErpAudit) -> anyhow::Result<()> {
    sock.set_nodelay(true)?;
    let mut reader = BufReader::new(sock);
    loop {
        let Some(head) = read_head(&mut reader).await? else {
            break; // client closed
        };
        // Drain the request body (Content-Length) so the stream stays framed
        // for the next keep-alive request.
        let len = content_length(&head);
        if len > 0 {
            let mut body = vec![0u8; len];
            reader.read_exact(&mut body).await?;
        }
        let (method, path) = request_line(&head);
        let response = if method == "GET" && path == "/audit" {
            let body = audit.snapshot().to_string();
            json_response(200, None, &body)
        } else if method == "POST" {
            let key = header_of(&head, "idempotency-key");
            let (status, retry_after) = audit.record(key.as_deref());
            let body = serde_json::json!({
                "status": status,
                "idempotency-key": key,
            })
            .to_string();
            json_response(status, retry_after, &body)
        } else {
            json_response(404, None, "{\"error\":\"not-found\"}")
        };
        reader.get_mut().write_all(response.as_bytes()).await?;
        reader.get_mut().flush().await?;
    }
    Ok(())
}

/// The reason phrase for the statuses this simulator emits.
fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        404 => "Not Found",
        429 => "Too Many Requests",
        _ => "OK",
    }
}

/// Build a framed HTTP/1.1 response with an optional integer-seconds
/// `Retry-After` header (the only shape the http node parses).
fn json_response(status: u16, retry_after_secs: Option<u64>, body: &str) -> String {
    let mut head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n",
        reason(status),
        body.len(),
    );
    if let Some(secs) = retry_after_secs {
        head.push_str(&format!("Retry-After: {secs}\r\n"));
    }
    head.push_str("\r\n");
    head.push_str(body);
    head
}

/// Read request head lines up to the blank line; `None` at EOF.
async fn read_head(reader: &mut BufReader<TcpStream>) -> anyhow::Result<Option<Vec<String>>> {
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

/// `(METHOD, path)` from the request line (first head line).
fn request_line(head: &[String]) -> (String, String) {
    let mut parts = head
        .first()
        .map(|l| l.split_whitespace())
        .into_iter()
        .flatten();
    let method = parts.next().unwrap_or("").to_ascii_uppercase();
    let path = parts.next().unwrap_or("/").to_string();
    (method, path)
}

/// The `Content-Length` of a request head (0 when absent/unparseable).
fn content_length(head: &[String]) -> usize {
    header_of(head, "content-length")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0)
}

/// Case-insensitive header lookup over `Name: value` lines.
fn header_of(head: &[String], name: &str) -> Option<String> {
    for line in head {
        if let Some((k, v)) = line.split_once(':')
            && k.trim().eq_ignore_ascii_case(name)
        {
            return Some(v.trim().to_string());
        }
    }
    None
}

/// CLI entry: serve the simulator forever on `--port`.
pub async fn run(args: ErpSimArgs) -> anyhow::Result<()> {
    let audit = ErpAudit::new(args.fail_first_n, args.retry_after_secs);
    serve(audit, args.port).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The core throttle sequence per key: K 429s (each with the retry-after),
    /// then exactly one effective 202, then idempotent 202 replays that record
    /// NO further delivery — the exactly-once-effective-callback contract.
    #[test]
    fn per_key_429_then_one_effective_then_idempotent_replays() {
        let audit = ErpAudit::new(2, 3);
        let key = Some("disp:evt:5:callback:0");

        assert_eq!(audit.record(key), (429, Some(3)));
        assert_eq!(audit.record(key), (429, Some(3)));
        assert_eq!(audit.record(key), (202, None), "the K+1th is the delivery");
        assert_eq!(audit.record(key), (202, None), "a duplicate replays 202");

        let rec = audit.key("disp:evt:5:callback:0");
        assert_eq!(rec.requests, 4);
        assert_eq!(rec.rejected_429, 2, "exactly K rejections");
        assert_eq!(rec.delivered, 1, "exactly ONE effective delivery per key");
        assert_eq!(audit.total_deliveries(), 1);
    }

    /// Distinct keys are independent — each parks its own K times, each delivers
    /// exactly once (the N-concurrent no-duplicate property).
    #[test]
    fn distinct_keys_are_independent() {
        let audit = ErpAudit::new(1, 2);
        for k in ["a", "b", "c"] {
            assert_eq!(audit.record(Some(k)), (429, Some(2)));
        }
        for k in ["a", "b", "c"] {
            assert_eq!(audit.record(Some(k)), (202, None));
        }
        assert_eq!(audit.distinct_keys(), 3);
        assert_eq!(audit.total_deliveries(), 3, "one delivery per distinct key");
        for k in ["a", "b", "c"] {
            assert_eq!(audit.key(k).delivered, 1);
            assert_eq!(audit.key(k).rejected_429, 1);
        }
    }

    /// `--fail-first-n 0` accepts immediately (no throttle) — one delivery, no 429.
    #[test]
    fn fail_first_zero_accepts_immediately() {
        let audit = ErpAudit::new(0, 5);
        assert_eq!(audit.record(Some("k")), (202, None));
        assert_eq!(audit.key("k").rejected_429, 0);
        assert_eq!(audit.key("k").delivered, 1);
    }
}
