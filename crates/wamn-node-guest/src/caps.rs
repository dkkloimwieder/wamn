//! The capability-bearing twin of the no-caps scaffolding (SR2): the
//! `wamn_node_sdk::NodeCtx` facade a component SHELL implements over its real
//! imports — `wamn:postgres` for data, outbound `wasi:http` for egress — plus
//! the WIT↔SDK value mirrors both directions. `components/flowrunner` grew
//! the first copy of this glue; this module is where it lives so the next
//! capability-bearing component links it instead of copying it.
//!
//! Feature-gated (`caps`) so the default build stays exactly the zero-import
//! scaffolding: a custom node built on `export_node!` alone must remain
//! physically incapable of I/O (the `world node` claim egressbench pins).
//! A component using [`CapsCtx`] must import `wamn:postgres/{types,client}`
//! and `wasi:http/{types,outgoing-handler}` at the versions pinned in
//! `wit-caps/world.wit` in its OWN world — the bindings here emit the same
//! canonical import names, so they unify at componentization.

use wamn_node_sdk as sdk;

mod bindings {
    wit_bindgen::generate!({
        world: "caps-node",
        path: "wit-caps",
        generate_all,
    });
}

use bindings::wamn::node::credentials as wit_credentials;
use bindings::wamn::postgres::client;
use bindings::wamn::postgres::types::{PgError, SqlValue};
use bindings::wasi::http::outgoing_handler;
use bindings::wasi::http::types::{
    ErrorCode, Fields, IncomingResponse, Method, OutgoingBody, OutgoingRequest, Scheme,
};

/// The component-shell capability facade: dispatch `wamn-nodes` (or any
/// SDK-authored node) over the component's real imports. The D8 raw-SQL flag
/// defaults OFF — per-project enablement wiring lands with the user-SQL role
/// split (wamn-1nd).
///
/// Constructed FRESH per node dispatch: `credential` carries ONLY the
/// executing node's declared credential name (`node.credential` in the flow),
/// which is what makes "the secret is injected only into the executing node's
/// context" structural — a sibling node's ctx never names it, so its
/// `credential()` is `NotGranted` without the vault ever being asked.
#[derive(Default)]
pub struct CapsCtx {
    /// Whether the `RawSql` capability is granted (D8; default off).
    pub raw_sql: bool,
    /// The executing node's DECLARED credential name (5.9). `None` = the node
    /// declared none; `credential()` refuses without a host call.
    pub credential: Option<String>,
}

impl sdk::NodeCtx for CapsCtx {
    fn http(&mut self, req: &sdk::HttpRequest) -> Result<sdk::HttpResponse, sdk::HttpCapError> {
        http_request_full(req)
    }

    fn pg_query(
        &mut self,
        sql: &str,
        params: &[sdk::PgValue],
    ) -> Result<sdk::PgRows, sdk::PgCapError> {
        let params: Vec<SqlValue> = params.iter().map(sdk_to_wit).collect();
        let rs = client::query(sql, &params).map_err(wit_err_to_sdk)?;
        Ok(sdk::PgRows {
            columns: rs.columns.iter().map(|c| c.name.clone()).collect(),
            rows: rs
                .rows
                .iter()
                .map(|r| r.iter().map(wit_to_sdk).collect())
                .collect(),
        })
    }

    fn pg_execute(&mut self, sql: &str, params: &[sdk::PgValue]) -> Result<u64, sdk::PgCapError> {
        let params: Vec<SqlValue> = params.iter().map(sdk_to_wit).collect();
        client::execute(sql, &params).map_err(wit_err_to_sdk)
    }

    fn catalog_json(&mut self) -> Result<String, sdk::PgCapError> {
        // The published project snapshot the api-gateway also reads (4.1b);
        // unqualified, resolved through the host-injected search_path.
        let rs = client::query("SELECT document::text FROM wamn_catalog LIMIT 1", &[])
            .map_err(wit_err_to_sdk)?;
        match rs.rows.first().and_then(|r| r.first()) {
            Some(SqlValue::Text(s)) | Some(SqlValue::Json(s)) => Ok(s.clone()),
            _ => Err(sdk::PgCapError::QueryError {
                code: String::new(),
                message: "no catalog snapshot published for this project".into(),
            }),
        }
    }

    fn raw_sql_enabled(&self) -> bool {
        self.raw_sql
    }

    fn credential(&mut self) -> Result<String, sdk::CredentialCapError> {
        // Only the DECLARED name ever reaches the host: no declaration, no
        // vault call. The host resolves the handle within the component's
        // project scope and audit-logs every get.
        let Some(name) = self.credential.as_deref() else {
            return Err(sdk::CredentialCapError::NotGranted);
        };
        wit_credentials::get(name).map_err(|e| match e {
            wit_credentials::CredentialError::NotGranted => sdk::CredentialCapError::NotGranted,
            wit_credentials::CredentialError::NotFound => sdk::CredentialCapError::NotFound,
            wit_credentials::CredentialError::Unavailable => sdk::CredentialCapError::Unavailable,
        })
    }
}

/// SDK value → binding value (both are 1:1 mirrors of the WIT `sql-value`).
fn sdk_to_wit(v: &sdk::PgValue) -> SqlValue {
    match v {
        sdk::PgValue::Null => SqlValue::Null,
        sdk::PgValue::Bool(b) => SqlValue::Boolean(*b),
        sdk::PgValue::Int32(n) => SqlValue::Int32(*n),
        sdk::PgValue::Int64(n) => SqlValue::Int64(*n),
        sdk::PgValue::Float64(f) => SqlValue::Float64(*f),
        sdk::PgValue::Text(s) => SqlValue::Text(s.clone()),
        sdk::PgValue::Bytes(b) => SqlValue::Bytes(b.clone()),
        sdk::PgValue::Numeric(s) => SqlValue::Numeric(s.clone()),
        sdk::PgValue::Timestamptz(s) => SqlValue::Timestamptz(s.clone()),
        sdk::PgValue::Json(s) => SqlValue::Json(s.clone()),
        sdk::PgValue::Uuid(s) => SqlValue::Uuid(s.clone()),
    }
}

/// Binding value → SDK value.
fn wit_to_sdk(v: &SqlValue) -> sdk::PgValue {
    match v {
        SqlValue::Null => sdk::PgValue::Null,
        SqlValue::Boolean(b) => sdk::PgValue::Bool(*b),
        SqlValue::Int32(n) => sdk::PgValue::Int32(*n),
        SqlValue::Int64(n) => sdk::PgValue::Int64(*n),
        SqlValue::Float64(f) => sdk::PgValue::Float64(*f),
        SqlValue::Text(s) => sdk::PgValue::Text(s.clone()),
        SqlValue::Bytes(b) => sdk::PgValue::Bytes(b.clone()),
        SqlValue::Numeric(s) => sdk::PgValue::Numeric(s.clone()),
        SqlValue::Timestamptz(s) => sdk::PgValue::Timestamptz(s.clone()),
        SqlValue::Json(s) => sdk::PgValue::Json(s.clone()),
        SqlValue::Uuid(s) => sdk::PgValue::Uuid(s.clone()),
    }
}

/// Binding pg-error → SDK capability error (1:1; the node classifies).
fn wit_err_to_sdk(e: PgError) -> sdk::PgCapError {
    match e {
        PgError::SerializationFailure => sdk::PgCapError::SerializationFailure,
        PgError::ConnectionUnavailable => sdk::PgCapError::ConnectionUnavailable,
        PgError::StatementTimeout => sdk::PgCapError::StatementTimeout,
        PgError::RowLimitExceeded(n) => sdk::PgCapError::RowLimitExceeded(n),
        PgError::UniqueViolation(c) => sdk::PgCapError::UniqueViolation(c),
        PgError::ForeignKeyViolation(c) => sdk::PgCapError::ForeignKeyViolation(c),
        PgError::CheckViolation(c) => sdk::PgCapError::CheckViolation(c),
        PgError::PermissionDenied => sdk::PgCapError::PermissionDenied,
        PgError::QueryError((code, message)) => sdk::PgCapError::QueryError { code, message },
    }
}

/// Full outbound request for the standard `http-request` node: method,
/// headers, body, https — and the response body drained completely. Egress
/// leaves the component ONLY via `wasi:http`, so the S6 egress spy (and the
/// production allowed_hosts policy) interposes here.
fn http_request_full(req: &sdk::HttpRequest) -> Result<sdk::HttpResponse, sdk::HttpCapError> {
    let (scheme, authority, path) = parse_url_any(&req.url)
        .ok_or_else(|| sdk::HttpCapError::BadRequest(format!("unparseable url {:?}", req.url)))?;
    let fields = Fields::new();
    for (k, v) in &req.headers {
        fields
            .append(k, &v.clone().into_bytes())
            .map_err(|e| sdk::HttpCapError::BadRequest(format!("header {k:?}: {e:?}")))?;
    }
    let out = OutgoingRequest::new(fields);
    if out.set_method(&wasi_method(&req.method)).is_err()
        || out.set_scheme(Some(&scheme)).is_err()
        || out.set_authority(Some(&authority)).is_err()
        || out.set_path_with_query(Some(&path)).is_err()
    {
        return Err(sdk::HttpCapError::BadRequest(
            "request fields rejected".into(),
        ));
    }
    let body = out
        .body()
        .map_err(|_| sdk::HttpCapError::BadRequest("body unavailable".into()))?;
    if let Some(bytes) = &req.body {
        let stream = body
            .write()
            .map_err(|_| sdk::HttpCapError::BadRequest("body stream unavailable".into()))?;
        // blocking_write_and_flush accepts at most 4096 bytes per call.
        for chunk in bytes.chunks(4096) {
            if stream.blocking_write_and_flush(chunk).is_err() {
                return Err(sdk::HttpCapError::Transport(
                    "request body write failed".into(),
                ));
            }
        }
    }
    if OutgoingBody::finish(body, None).is_err() {
        return Err(sdk::HttpCapError::Transport(
            "request body finish failed".into(),
        ));
    }
    let fut = match outgoing_handler::handle(out, None) {
        Ok(f) => f,
        Err(code) => return Err(handle_err(code)), // host refused before dispatch
    };
    let pollable = fut.subscribe();
    pollable.block();
    match fut.get() {
        Some(Ok(Ok(resp))) => {
            let status = resp.status();
            let headers = resp
                .headers()
                .entries()
                .into_iter()
                .map(|(k, v)| (k, String::from_utf8_lossy(&v).into_owned()))
                .collect();
            let body = read_incoming_body(resp)?;
            Ok(sdk::HttpResponse {
                status,
                headers,
                body,
            })
        }
        Some(Ok(Err(code))) => Err(handle_err(code)),
        _ => Err(sdk::HttpCapError::Transport("no response".into())),
    }
}

/// A `wasi:http` refusal → SDK capability error: an explicit host denial (the
/// allowedHosts policy / the S6 egress spy) is permanent; anything else is a
/// transport failure the node classifies as retryable.
fn handle_err(code: ErrorCode) -> sdk::HttpCapError {
    match code {
        ErrorCode::HttpRequestDenied => sdk::HttpCapError::Denied,
        other => sdk::HttpCapError::Transport(format!("{other:?}")),
    }
}

fn read_incoming_body(resp: IncomingResponse) -> Result<Vec<u8>, sdk::HttpCapError> {
    let body = resp
        .consume()
        .map_err(|_| sdk::HttpCapError::Transport("body already consumed".into()))?;
    let stream = body
        .stream()
        .map_err(|_| sdk::HttpCapError::Transport("body stream unavailable".into()))?;
    let mut out = Vec::new();
    loop {
        match stream.blocking_read(64 * 1024) {
            Ok(chunk) => out.extend_from_slice(&chunk),
            Err(bindings::wasi::io::streams::StreamError::Closed) => break,
            Err(e) => {
                return Err(sdk::HttpCapError::Transport(format!("body read: {e:?}")));
            }
        }
    }
    Ok(out)
}

fn parse_url_any(url: &str) -> Option<(Scheme, String, String)> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("http://") {
        (Scheme::Http, r)
    } else {
        (Scheme::Https, url.strip_prefix("https://")?)
    };
    let split = rest.find(['/', '?']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(split);
    if authority.is_empty() {
        return None;
    }
    let path = if tail.is_empty() {
        "/".to_string()
    } else if tail.starts_with('?') {
        format!("/{tail}")
    } else {
        tail.to_string()
    };
    Some((scheme, authority.to_string(), path))
}

fn wasi_method(m: &str) -> Method {
    match m {
        "GET" => Method::Get,
        "HEAD" => Method::Head,
        "POST" => Method::Post,
        "PUT" => Method::Put,
        "DELETE" => Method::Delete,
        "CONNECT" => Method::Connect,
        "OPTIONS" => Method::Options,
        "TRACE" => Method::Trace,
        "PATCH" => Method::Patch,
        other => Method::Other(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every sql-value variant survives the SDK->WIT->SDK round trip — the
    /// two mirrors cannot drift apart variant-for-variant.
    #[test]
    fn sql_value_mirrors_round_trip() {
        let vals = [
            sdk::PgValue::Null,
            sdk::PgValue::Bool(true),
            sdk::PgValue::Int32(-7),
            sdk::PgValue::Int64(1 << 40),
            sdk::PgValue::Float64(2.5),
            sdk::PgValue::Text("t".into()),
            sdk::PgValue::Bytes(vec![1, 2]),
            sdk::PgValue::Numeric("12.50".into()),
            sdk::PgValue::Timestamptz("2026-01-01T00:00:00Z".into()),
            sdk::PgValue::Json("{\"a\":1}".into()),
            sdk::PgValue::Uuid("a0000000-0000-0000-0000-000000000001".into()),
        ];
        for v in &vals {
            assert_eq!(&wit_to_sdk(&sdk_to_wit(v)), v);
        }
    }

    /// The pg-error map is 1:1 (the node classifies; this glue never does).
    #[test]
    fn pg_error_maps_variant_for_variant() {
        assert!(matches!(
            wit_err_to_sdk(PgError::SerializationFailure),
            sdk::PgCapError::SerializationFailure
        ));
        assert!(matches!(
            wit_err_to_sdk(PgError::StatementTimeout),
            sdk::PgCapError::StatementTimeout
        ));
        assert!(matches!(
            wit_err_to_sdk(PgError::UniqueViolation("c".into())),
            sdk::PgCapError::UniqueViolation(c) if c == "c"
        ));
        assert!(matches!(
            wit_err_to_sdk(PgError::QueryError(("22P02".into(), "m".into()))),
            sdk::PgCapError::QueryError { code, .. } if code == "22P02"
        ));
    }

    /// The per-dispatch credential scoping is LOCAL and fail-closed: a ctx
    /// whose node declared no credential refuses without ever calling the
    /// host vault import (this test runs on the host target, where a real
    /// `credentials.get` call would abort — not returning `NotGranted` here
    /// means the guard is gone).
    #[test]
    fn credential_without_a_declaration_is_not_granted_locally() {
        use sdk::NodeCtx as _;
        let mut ctx = CapsCtx::default();
        assert_eq!(ctx.credential(), Err(sdk::CredentialCapError::NotGranted));
    }

    #[test]
    fn url_parse_covers_scheme_authority_path() {
        let (s, a, p) = parse_url_any("https://api.example:8443/v1?x=1").unwrap();
        assert!(matches!(s, Scheme::Https));
        assert_eq!(a, "api.example:8443");
        assert_eq!(p, "/v1?x=1");
        let (s, a, p) = parse_url_any("http://h").unwrap();
        assert!(matches!(s, Scheme::Http));
        assert_eq!(a, "h");
        assert_eq!(p, "/");
        assert!(parse_url_any("ftp://x").is_none());
    }
}
