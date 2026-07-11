//! Per-project generated REST API gateway component (platform-plan 4.1).
//!
//! The thin `wasi:http` ⇆ `wamn:postgres` shell around the pure `wamn-api`
//! crate: parse the incoming HTTP request, load the project's catalog snapshot
//! from the database (memoized), compile the request to injection-safe
//! parameterized SQL, run it through the host `wamn:postgres` capability under
//! the injected `app.tenant` claim, and shape the row-set into a JSON response.
//!
//! All routing/SQL/shaping logic — and every safety invariant (values are `$n`
//! params, identifiers are catalog-allowlisted, `tenant_id` is set server-side,
//! numeric stays an exact-decimal string) — lives in `wamn-api`, which is
//! exhaustively unit-tested with no host and no database. This file only moves
//! bytes across the two capability boundaries.

wit_bindgen::generate!({
    world: "api-gateway",
    path: "wit",
    generate_all,
});

use std::collections::HashSet;
use std::sync::OnceLock;

use serde_json::{Value, json};

use exports::wasi::http::incoming_handler::Guest;
use wamn::postgres::client;
use wamn::postgres::types::{PgError, RowSet, SqlValue as PgVal};
use wamn_api::{
    Catalog, Compiled, Expand, Method as ApiMethod, Plan, PlanKind, Router, SqlValue as ApiVal,
    attach_expansion, shape_rows,
};
use wasi::http::types::{
    Fields, IncomingRequest, Method as HttpMethod, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use wasi::io::streams::StreamError;

struct Component;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let (status, body) = process(&request);
        send_response(response_out, status, body);
    }
}

export!(Component);

/// The project's catalog snapshot, loaded once from the database and memoized
/// for the lifetime of the instance (the 4.4 hot-reload doorbell will refresh
/// it; v1 reads it once). `OnceLock` because a Wasm component is single-threaded.
static CATALOG: OnceLock<Catalog> = OnceLock::new();

// ---- request pipeline -----------------------------------------------------

/// Run one request end to end, returning `(status, body?)`.
fn process(request: &IncomingRequest) -> (u16, Option<Vec<u8>>) {
    let Some(method) = map_method(&request.method()) else {
        return error(405, "method-not-allowed", "method not supported");
    };
    let target = request.path_with_query().unwrap_or_default();
    let (path, query) = parse_target(&target);

    let raw_body = read_body(request);
    let body_json = if raw_body.is_empty() {
        None
    } else {
        match serde_json::from_slice::<Value>(&raw_body) {
            Ok(v) => Some(v),
            Err(_) => return error(400, "invalid-json", "request body is not valid JSON"),
        }
    };

    let catalog = match catalog() {
        Ok(c) => c,
        Err(e) => return error(503, "catalog-unavailable", &e),
    };
    let router = Router::new(catalog);

    let plan = match router.compile(method, &path, &query, body_json.as_ref()) {
        Ok(p) => p,
        Err(e) => return (e.status(), Some(serialize(&e.to_json()))),
    };

    match execute(&router, &plan) {
        Ok((status, Some(v))) => (status, Some(serialize(&v))),
        Ok((status, None)) => (status, None),
        Err((status, code, msg)) => error(status, &code, &msg),
    }
}

/// Execute a compiled plan against the database and shape the result.
fn execute(router: &Router, plan: &Plan) -> Result<(u16, Option<Value>), (u16, String, String)> {
    match plan.kind {
        PlanKind::List => {
            let rs = run_query(&plan.query)?;
            let mut rows = shape_rows(&plan.query.columns, &api_rows(&rs));
            apply_expands(router, &mut rows, &plan.query.columns, &rs, &plan.expands)?;
            Ok((plan.status, Some(Value::Array(rows))))
        }
        PlanKind::GetOne => {
            let rs = run_query(&plan.query)?;
            if rs.rows.is_empty() {
                return Ok((404, Some(not_found())));
            }
            let mut rows = shape_rows(&plan.query.columns, &api_rows(&rs));
            apply_expands(router, &mut rows, &plan.query.columns, &rs, &plan.expands)?;
            Ok((plan.status, rows.into_iter().next()))
        }
        PlanKind::CreateOne => {
            let rs = run_query(&plan.query)?;
            let row = shape_rows(&plan.query.columns, &api_rows(&rs)).into_iter().next();
            Ok((plan.status, Some(row.unwrap_or(Value::Null))))
        }
        PlanKind::UpdateOne => {
            let rs = run_query(&plan.query)?;
            if rs.rows.is_empty() {
                return Ok((404, Some(not_found())));
            }
            let row = shape_rows(&plan.query.columns, &api_rows(&rs)).into_iter().next();
            Ok((plan.status, Some(row.unwrap_or(Value::Null))))
        }
        PlanKind::DeleteOne => {
            let rs = run_query(&plan.query)?;
            if rs.rows.is_empty() {
                return Ok((404, Some(not_found())));
            }
            Ok((plan.status, None)) // 204, no body
        }
    }
}

/// Run the primary or an expansion query through `wamn:postgres`.
fn run_query(c: &Compiled) -> Result<RowSet, (u16, String, String)> {
    client::query(&c.sql, &pg_params(&c.params)).map_err(map_pg_error)
}

/// Attach every one-level expansion to the already-shaped primary rows: gather
/// the distinct join keys, run one `IN (…)` query per relation, then merge.
fn apply_expands(
    router: &Router,
    rows: &mut [Value],
    columns: &[String],
    primary: &RowSet,
    expands: &[Expand],
) -> Result<(), (u16, String, String)> {
    for ex in expands {
        let Some(key_idx) = columns.iter().position(|c| c == &ex.key_column) else {
            continue;
        };
        let mut seen = HashSet::new();
        let mut keys: Vec<ApiVal> = Vec::new();
        for row in &primary.rows {
            if let Some(cell) = row.get(key_idx) {
                let v = from_pg(cell);
                if !matches!(v, ApiVal::Null) && seen.insert(v.group_key()) {
                    keys.push(v);
                }
            }
        }
        if keys.is_empty() {
            attach_expansion(rows, ex, &ex.columns, &[]);
            continue;
        }
        let sub = router.build_expand(ex, &keys);
        let rs = run_query(&sub)?;
        attach_expansion(rows, ex, &ex.columns, &api_rows(&rs));
    }
    Ok(())
}

// ---- catalog snapshot -----------------------------------------------------

fn catalog() -> Result<&'static Catalog, String> {
    if let Some(c) = CATALOG.get() {
        return Ok(c);
    }
    let json = load_catalog_json()?;
    let cat = Catalog::from_json(&json).map_err(|e| format!("catalog snapshot parse error: {e}"))?;
    let _ = CATALOG.set(cat);
    CATALOG.get().ok_or_else(|| "catalog init race".to_string())
}

/// Read the single catalog snapshot row for this project. RLS scopes the read
/// to the injected tenant; the table name is unqualified (the host injects the
/// project schema via `search_path`, exactly like every other query).
fn load_catalog_json() -> Result<String, String> {
    let rs = client::query("SELECT document::text FROM wamn_catalog LIMIT 1", &[]).map_err(|e| {
        let (_, code, msg) = map_pg_error(e);
        format!("catalog load: {code}: {msg}")
    })?;
    let row = rs.rows.first().ok_or_else(|| "no catalog snapshot for this project".to_string())?;
    match row.first() {
        Some(PgVal::Text(s)) | Some(PgVal::Json(s)) => Ok(s.clone()),
        other => Err(format!("unexpected catalog document shape: {other:?}")),
    }
}

// ---- value / error mapping ------------------------------------------------

fn pg_params(params: &[ApiVal]) -> Vec<PgVal> {
    params.iter().map(to_pg).collect()
}

fn api_rows(rs: &RowSet) -> Vec<Vec<ApiVal>> {
    rs.rows.iter().map(|r| r.iter().map(from_pg).collect()).collect()
}

/// `wamn-api` value → `wamn:postgres` binding value (1:1).
fn to_pg(v: &ApiVal) -> PgVal {
    match v {
        ApiVal::Null => PgVal::Null,
        ApiVal::Bool(b) => PgVal::Boolean(*b),
        ApiVal::Int32(n) => PgVal::Int32(*n),
        ApiVal::Int64(n) => PgVal::Int64(*n),
        ApiVal::Float64(f) => PgVal::Float64(*f),
        ApiVal::Text(s) => PgVal::Text(s.clone()),
        ApiVal::Bytes(b) => PgVal::Bytes(b.clone()),
        ApiVal::Numeric(s) => PgVal::Numeric(s.clone()),
        ApiVal::Timestamptz(s) => PgVal::Timestamptz(s.clone()),
        ApiVal::Json(s) => PgVal::Json(s.clone()),
        ApiVal::Uuid(s) => PgVal::Uuid(s.clone()),
    }
}

/// `wamn:postgres` binding value → `wamn-api` value (1:1).
fn from_pg(v: &PgVal) -> ApiVal {
    match v {
        PgVal::Null => ApiVal::Null,
        PgVal::Boolean(b) => ApiVal::Bool(*b),
        PgVal::Int32(n) => ApiVal::Int32(*n),
        PgVal::Int64(n) => ApiVal::Int64(*n),
        PgVal::Float64(f) => ApiVal::Float64(*f),
        PgVal::Text(s) => ApiVal::Text(s.clone()),
        PgVal::Bytes(b) => ApiVal::Bytes(b.clone()),
        PgVal::Numeric(s) => ApiVal::Numeric(s.clone()),
        PgVal::Timestamptz(s) => ApiVal::Timestamptz(s.clone()),
        PgVal::Json(s) => ApiVal::Json(s.clone()),
        PgVal::Uuid(s) => ApiVal::Uuid(s.clone()),
    }
}

/// Map a `pg-error` onto an HTTP status + error code + message.
fn map_pg_error(e: PgError) -> (u16, String, String) {
    match e {
        PgError::PermissionDenied => (403, "permission-denied".into(), "permission denied".into()),
        PgError::UniqueViolation(c) => (409, "unique-violation".into(), format!("unique constraint: {c}")),
        PgError::ForeignKeyViolation(c) => {
            (409, "foreign-key-violation".into(), format!("foreign key: {c}"))
        }
        PgError::CheckViolation(c) => (409, "check-violation".into(), format!("check constraint: {c}")),
        PgError::SerializationFailure => {
            (409, "serialization-failure".into(), "write conflict, retry".into())
        }
        PgError::StatementTimeout => (503, "statement-timeout".into(), "statement timed out".into()),
        PgError::ConnectionUnavailable => {
            (503, "connection-unavailable".into(), "database unavailable".into())
        }
        PgError::RowLimitExceeded(n) => {
            (400, "row-limit-exceeded".into(), format!("row limit {n} exceeded"))
        }
        PgError::QueryError((code, msg)) => (400, "query-error".into(), format!("{code}: {msg}")),
    }
}

// ---- HTTP plumbing --------------------------------------------------------

fn map_method(m: &HttpMethod) -> Option<ApiMethod> {
    match m {
        HttpMethod::Get => Some(ApiMethod::Get),
        HttpMethod::Post => Some(ApiMethod::Post),
        HttpMethod::Put => Some(ApiMethod::Put),
        HttpMethod::Patch => Some(ApiMethod::Patch),
        HttpMethod::Delete => Some(ApiMethod::Delete),
        HttpMethod::Other(s) => ApiMethod::from_http(s),
        _ => None,
    }
}

/// Split `path?query` and decode the query pairs.
fn parse_target(target: &str) -> (String, Vec<(String, String)>) {
    match target.split_once('?') {
        Some((path, qs)) => (path.to_string(), parse_query(qs)),
        None => (target.to_string(), Vec::new()),
    }
}

fn parse_query(qs: &str) -> Vec<(String, String)> {
    qs.split('&')
        .filter(|s| !s.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) => (percent_decode(k), percent_decode(v)),
            None => (percent_decode(pair), String::new()),
        })
        .collect()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => match (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push(h * 16 + l);
                    i += 3;
                }
                _ => {
                    out.push(b'%');
                    i += 1;
                }
            },
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Read the whole request body (v1 does not cap the size — that is 4.6).
fn read_body(request: &IncomingRequest) -> Vec<u8> {
    let Ok(body) = request.consume() else {
        return Vec::new();
    };
    let mut buf = Vec::new();
    if let Ok(stream) = body.stream() {
        loop {
            match stream.blocking_read(8192) {
                Ok(chunk) if chunk.is_empty() => break,
                Ok(chunk) => buf.extend_from_slice(&chunk),
                Err(StreamError::Closed) => break,
                Err(_) => break,
            }
        }
    }
    buf
}

/// Send the HTTP response: set the outparam, then stream the JSON body in
/// ≤4 KiB chunks (the `blocking-write-and-flush` limit).
fn send_response(response_out: ResponseOutparam, status: u16, body: Option<Vec<u8>>) {
    let headers = Fields::new();
    if body.is_some() {
        let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
    }
    let resp = OutgoingResponse::new(headers);
    let _ = resp.set_status_code(status);
    let outgoing_body = resp.body().expect("outgoing-response body");
    ResponseOutparam::set(response_out, Ok(resp));

    if let Some(bytes) = body {
        if let Ok(stream) = outgoing_body.write() {
            for chunk in bytes.chunks(4096) {
                if stream.blocking_write_and_flush(chunk).is_err() {
                    break;
                }
            }
        }
    }
    let _ = OutgoingBody::finish(outgoing_body, None);
}

// ---- small helpers --------------------------------------------------------

fn serialize(v: &Value) -> Vec<u8> {
    serde_json::to_vec(v).unwrap_or_default()
}

fn error(status: u16, code: &str, message: &str) -> (u16, Option<Vec<u8>>) {
    (status, Some(serialize(&json!({ "error": { "code": code, "message": message } }))))
}

fn not_found() -> Value {
    json!({ "error": { "code": "not-found", "message": "not found" } })
}
