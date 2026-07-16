//! `http-request` — one outbound HTTP call through the runner's `wasi:http`
//! capability (still under the host's `allowedHosts` policy).
//!
//! Config:
//! ```jsonc
//! {
//!   "method": "POST",                        // default GET
//!   "url": "https://api.example/x/{{id}}",   // {{jmespath}} templating
//!   "headers": {"x-token": "{{auth.token}}"},// values templated
//!   "body": "payload",                       // OPTIONAL jmespath over the
//!                                            // input; null result = no body;
//!                                            // else sent as JSON
//!   "credential-header": "x-api-key"         // OPTIONAL header the node's
//!                                            // DECLARED credential (5.9) is
//!                                            // sent as; default authorization
//! }
//! ```
//! Success payload: `{"status": n, "headers": {...}, "body": <json-or-string>}`.
//!
//! The status → taxonomy map is MECHANICAL ([`classify_response`]): 429 →
//! `rate-limited` (Retry-After honored, throttle keyed by the target host);
//! 408/5xx → `retryable`; other 4xx → `terminal`; transport failure with no
//! response → `retryable`; a host egress denial → `terminal` (policy does not
//! heal). 3xx is NOT followed — it lands in the success payload.

use serde_json::{Map, Value};
use wamn_node_sdk::{
    Capability, CredentialCapError, Emission, ErrorDetail, HttpCapError, HttpRequest, HttpResponse,
    Node, NodeCtx, NodeError, RateLimitDetail, RunContext,
};

use crate::expr::eval_to_value;
use crate::template::expand;

pub(crate) struct HttpRequestNode;

impl Node for HttpRequestNode {
    fn capabilities(&self) -> &'static [Capability] {
        &[Capability::HttpEgress]
    }

    fn run(
        &self,
        ctx: &mut dyn NodeCtx,
        run: &RunContext<'_>,
        input: &Value,
    ) -> Result<Emission, NodeError> {
        let mut req = build_request(run.config, input)?;
        // 9.2: forward the active W3C trace context so this request continues
        // the run's trace. The host also stamps outbound `wasi:http` calls, so
        // continuity holds regardless; forwarding here keeps `traceparent`
        // present on the node's own request (a config header of the same name
        // still wins — `apply_trace_context` skips keys already set).
        run.apply_trace_context(&mut req.headers);
        // 5.9: the node's DECLARED credential (`node.credential` in the flow)
        // resolves through the vault and rides as a header. The secret never
        // touches config or flow data — it exists only in this request.
        apply_credential(ctx, run.config, &mut req.headers)?;
        let host = url_host(&req.url).unwrap_or_default().to_string();
        match ctx.http(&req) {
            Ok(resp) => classify_response(&host, &resp),
            Err(e) => Err(classify_cap_error(e)),
        }
    }
}

/// Build the outbound request from config + input (pure).
pub(crate) fn build_request(config: &Value, input: &Value) -> Result<HttpRequest, NodeError> {
    let url_template = config.get("url").and_then(Value::as_str).ok_or_else(|| {
        NodeError::Terminal(ErrorDetail::coded(
            "invalid-config",
            "http-request config requires a string \"url\"",
        ))
    })?;
    let url = expand(url_template, input)?;
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(NodeError::Terminal(ErrorDetail::coded(
            "invalid-config",
            format!("http-request url must be absolute http(s), got {url:?}"),
        )));
    }
    let method = config
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("GET")
        .to_ascii_uppercase();

    let mut headers: Vec<(String, String)> = Vec::new();
    if let Some(hs) = config.get("headers") {
        let obj = hs.as_object().ok_or_else(|| {
            NodeError::Terminal(ErrorDetail::coded(
                "invalid-config",
                "http-request \"headers\" must be an object of strings",
            ))
        })?;
        for (k, v) in obj {
            let raw = v.as_str().ok_or_else(|| {
                NodeError::Terminal(ErrorDetail::coded(
                    "invalid-config",
                    format!("http-request header {k:?} must be a string"),
                ))
            })?;
            headers.push((k.clone(), expand(raw, input)?));
        }
    }

    let body = match config.get("body").and_then(Value::as_str) {
        Some(expr) => match eval_to_value(expr, input)? {
            Value::Null => None,
            v => {
                if !headers
                    .iter()
                    .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                {
                    headers.push(("content-type".into(), "application/json".into()));
                }
                Some(v.to_string().into_bytes())
            }
        },
        None => None,
    };

    Ok(HttpRequest {
        method,
        url,
        headers,
        body,
    })
}

/// 5.9: resolve the node's declared credential and send it as a header.
/// Header name from config `"credential-header"` (default `"authorization"`);
/// an explicit config header of the same name wins (the trace-context rule).
/// `NotGranted` means no credential is in this node's context (none declared)
/// — not an error, the request proceeds bare. `not-found` is config-shaped
/// (terminal); `unavailable` is the backing store (retryable, per the WIT
/// annotation). The secret value never enters an error detail.
fn apply_credential(
    ctx: &mut dyn NodeCtx,
    config: &Value,
    headers: &mut Vec<(String, String)>,
) -> Result<(), NodeError> {
    let header = config
        .get("credential-header")
        .and_then(Value::as_str)
        .unwrap_or("authorization");
    match ctx.credential() {
        Ok(secret) => {
            if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(header)) {
                headers.push((header.to_string(), secret));
            }
            Ok(())
        }
        Err(CredentialCapError::NotGranted) => Ok(()),
        Err(CredentialCapError::NotFound) => Err(NodeError::Terminal(ErrorDetail::coded(
            "credential-not-found",
            "the node's declared credential is unknown in this project's vault",
        ))),
        Err(CredentialCapError::Unavailable) => Err(NodeError::Retryable(ErrorDetail::coded(
            "credential-unavailable",
            "the credential vault's backing store is unavailable",
        ))),
    }
}

/// The authority (host[:port]) of an absolute http(s) URL — the shared
/// throttle's target-host key.
pub(crate) fn url_host(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = &rest[..end];
    (!host.is_empty()).then_some(host)
}

/// Mechanical response → taxonomy classification (see module docs).
pub(crate) fn classify_response(host: &str, resp: &HttpResponse) -> Result<Emission, NodeError> {
    let status = resp.status;
    if status == 429 {
        let retry_after_ms = resp
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("retry-after"))
            .and_then(|(_, v)| v.trim().parse::<u64>().ok())
            .map(|secs| secs * 1000);
        return Err(NodeError::RateLimited(RateLimitDetail {
            detail: detail_for(status, resp),
            retry_after_ms,
            target_host: (!host.is_empty()).then(|| host.to_string()),
        }));
    }
    match status {
        408 | 500..=599 => Err(NodeError::Retryable(detail_for(status, resp))),
        400..=499 => Err(NodeError::Terminal(detail_for(status, resp))),
        _ => {
            let mut headers = Map::new();
            for (k, v) in &resp.headers {
                headers.insert(k.to_ascii_lowercase(), Value::String(v.clone()));
            }
            let body = parse_body(&resp.body);
            Ok(Emission::main(serde_json::json!({
                "status": status,
                "headers": headers,
                "body": body,
            })))
        }
    }
}

/// A capability-layer failure (no HTTP status existed) → taxonomy.
pub(crate) fn classify_cap_error(e: HttpCapError) -> NodeError {
    match e {
        HttpCapError::NotGranted => NodeError::Terminal(ErrorDetail::coded(
            "capability-denied",
            "http egress is not granted to this node",
        )),
        HttpCapError::Denied => NodeError::Terminal(ErrorDetail::coded(
            "egress-denied",
            "the host refused the egress (allowedHosts policy)",
        )),
        HttpCapError::BadRequest(m) => NodeError::Terminal(ErrorDetail::coded(
            "invalid-request",
            format!("the request could not be built: {m}"),
        )),
        HttpCapError::Transport(m) => NodeError::Retryable(ErrorDetail::coded(
            "http-transport",
            format!("transport failure: {m}"),
        )),
    }
}

/// Body bytes as JSON when they parse, else a (lossy) string.
fn parse_body(body: &[u8]) -> Value {
    serde_json::from_slice(body)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(body).into_owned()))
}

/// The `HTTP_<n>` detail, with a bounded body head in `data` for run history.
fn detail_for(status: u16, resp: &HttpResponse) -> ErrorDetail {
    let head = String::from_utf8_lossy(&resp.body[..resp.body.len().min(2048)]).into_owned();
    ErrorDetail {
        message: format!("upstream answered HTTP {status}"),
        code: Some(format!("HTTP_{status}")),
        data: Some(serde_json::json!({"status": status, "body": head})),
    }
}
