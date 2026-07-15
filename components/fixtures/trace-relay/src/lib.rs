//! trace-relay (9.2 traceproof fixture). See `wit/world.wit`.
//!
//! On any inbound request the relay makes ONE **bare** outbound GET (no trace
//! headers set by the guest) to an upstream and returns the upstream's body.
//! The host stamps `traceparent` onto the outbound call (9.2), so the upstream
//! receives the active trace even though the guest never touched a header —
//! that is the load-bearing property `traceproof` checks.

wit_bindgen::generate!({
    world: "trace-relay",
    path: "wit",
    generate_all,
});

use exports::wasi::http::incoming_handler::Guest;
use wasi::http::outgoing_handler;
use wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingRequest, OutgoingResponse,
    ResponseOutparam, Scheme,
};

/// Default upstream if the caller does not pin one via `x-relay-upstream`.
/// Overridden in the deploy manifest's expectations; a bare host:port URL.
const DEFAULT_UPSTREAM: &str = "http://serve-echo:8091/";

struct Component;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let upstream = header_value(&request, "x-relay-upstream")
            .unwrap_or_else(|| DEFAULT_UPSTREAM.to_string());
        // Drain the inbound body so the request is well-formed; ignore it.
        drain_incoming(&request);
        let (status, body) = relay_get(&upstream);
        send_response(response_out, status, &body);
    }
}

export!(Component);

/// First value of `name` from the incoming request headers, as a UTF-8 string.
fn header_value(request: &IncomingRequest, name: &str) -> Option<String> {
    let all = request.headers().get(name);
    all.into_iter()
        .next()
        .and_then(|v| String::from_utf8(v).ok())
}

/// Make one BARE outbound GET (the guest sets NO trace headers) and return
/// `(status, body)`. Status is 0 and body empty on any transport failure.
fn relay_get(url: &str) -> (u16, Vec<u8>) {
    let Some((scheme, authority, path)) = parse_http_url(url) else {
        return (0, Vec::new());
    };
    let req = OutgoingRequest::new(Fields::new());
    if req.set_method(&Method::Get).is_err()
        || req.set_scheme(Some(&scheme)).is_err()
        || req.set_authority(Some(&authority)).is_err()
        || req.set_path_with_query(Some(&path)).is_err()
    {
        return (0, Vec::new());
    }
    let fut = match outgoing_handler::handle(req, None) {
        Ok(f) => f,
        Err(_) => return (0, Vec::new()), // host refused before dispatch
    };
    let pollable = fut.subscribe();
    pollable.block();
    let resp = match fut.get() {
        Some(Ok(Ok(resp))) => resp,
        _ => return (0, Vec::new()),
    };
    let status = resp.status();
    let body = read_incoming_body(resp.consume().ok());
    (status, body)
}

/// Read an `IncomingBody`'s stream to end. `None` body => empty.
fn read_incoming_body(body: Option<wasi::http::types::IncomingBody>) -> Vec<u8> {
    let Some(body) = body else {
        return Vec::new();
    };
    let mut buf = Vec::new();
    if let Ok(stream) = body.stream() {
        loop {
            match stream.blocking_read(8192) {
                Ok(chunk) if chunk.is_empty() => break,
                Ok(chunk) => buf.extend_from_slice(&chunk),
                Err(_) => break,
            }
        }
    }
    buf
}

/// Consume the inbound request body (well-formedness; value ignored).
fn drain_incoming(request: &IncomingRequest) {
    if let Ok(body) = request.consume() {
        let _ = read_incoming_body(Some(body));
    }
}

fn parse_http_url(url: &str) -> Option<(Scheme, String, String)> {
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (rest[..i].to_string(), rest[i..].to_string()),
        None => (rest.to_string(), "/".to_string()),
    };
    if authority.is_empty() {
        return None;
    }
    Some((Scheme::Http, authority, path))
}

/// Return the upstream body verbatim (200 when the upstream answered, 502 when
/// it did not) so the `traceproof` client sees exactly what the upstream
/// reflected.
fn send_response(response_out: ResponseOutparam, upstream_status: u16, body: &[u8]) {
    let status = if upstream_status == 0 { 502 } else { 200 };
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    let resp = OutgoingResponse::new(headers);
    let _ = resp.set_status_code(status);
    let outgoing_body = resp.body().expect("outgoing-response body");
    ResponseOutparam::set(response_out, Ok(resp));

    if let Ok(stream) = outgoing_body.write() {
        for chunk in body.chunks(4096) {
            if stream.blocking_write_and_flush(chunk).is_err() {
                break;
            }
        }
    }
    let _ = OutgoingBody::finish(outgoing_body, None);
}
