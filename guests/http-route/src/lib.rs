//! HTTP adapter from released route data to typed application invocation.
#![cfg_attr(target_arch = "wasm32", expect(unsafe_code, reason = "wit-bindgen"))]

use serde::Serialize;
use serde_json::{Map, Value, json};

pub use bindings::wamn::flow_http_routing::routing::{AuthRejection, Header};

/// Cheap request metadata available before reading the body.
#[derive(Debug, Clone)]
pub struct RequestHead {
    pub method: String,
    pub authority: String,
    pub target: String,
    pub headers: Vec<Header>,
}

#[path = "../../../../../crates/catalog/model/src/http_route_data.rs"]
pub mod route_data;
pub use route_data::RouteDefinition;

/// Trace context forwarded unchanged to component execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceContext {
    pub traceparent: String,
    pub tracestate: Option<String>,
}

/// One request to the released application operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationRequest<Caller> {
    pub payload: String,
    pub caller: Option<Caller>,
    pub trace: Option<TraceContext>,
    /// The conditional read tag compared by the entry.
    pub if_none_match: Option<String>,
}

/// A bounded HTTP response produced by the adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
    /// The cache facts of a read that responded or was not modified. None is
    /// `no-store` with no ETag. Boxed, because every refusal is an
    /// `HttpResponse` too and most responses carry none.
    pub read: Option<Box<ReadCache>>,
}

/// The cache facts of one read response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadCache {
    /// The Cache-Control value that the host derived for the route. None is
    /// `no-store`, as for a request that carries the CSRF header.
    pub cache_control: Option<String>,
    /// The ETag that the host derived for the read.
    pub etag: Option<String>,
}

impl HttpResponse {
    /// The cache headers of this response. A successful read sends the value
    /// that the host derived for its route and varies on the caller's
    /// credentials. Every other response is `no-store`. A read with an ETag
    /// also sends it.
    #[must_use]
    pub fn cache_headers(&self) -> Vec<(&'static str, String)> {
        let read = self.read.as_deref();
        let mut headers = match read.and_then(|read| read.cache_control.as_ref()) {
            Some(value) => vec![
                ("cache-control", value.clone()),
                ("vary", "Authorization, Cookie".to_string()),
            ],
            None => vec![("cache-control", "no-store".to_string())],
        };
        if let Some(etag) = read.and_then(|read| read.etag.as_ref()) {
            headers.push(("etag", etag.clone()));
        }
        headers
    }
}

#[derive(Serialize)]
struct ErrorEnvelope<T> {
    error: T,
}

/// Adapter-owned body ceilings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdapterLimits {
    pub body_bytes: usize,
}

impl Default for AdapterLimits {
    fn default() -> Self {
        Self {
            body_bytes: 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodyReadError;

impl std::fmt::Display for BodyReadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("request body read failed")
    }
}

impl std::error::Error for BodyReadError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderError;

impl std::fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("flow HTTP provider failed")
    }
}

impl std::error::Error for ProviderError {}

/// The host refused the mapped payload against the route input schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaInvalid {
    /// The RFC 6901 pointer of the offending value. A platform-side cause has none.
    pub pointer: Option<String>,
}

impl SchemaInvalid {
    /// Read the host refusal. Only an empty string or a string that starts
    /// with `/` is a pointer. Any other string names a platform-side cause.
    #[must_use]
    pub fn from_refusal(refusal: String) -> Self {
        let pointer = (refusal.is_empty() || refusal.starts_with('/')).then_some(refusal);
        Self { pointer }
    }
}

impl std::fmt::Display for SchemaInvalid {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("route input schema refused the payload")
    }
}

impl std::error::Error for SchemaInvalid {}

/// Incremental request-body source. A read error prevents invocation.
#[expect(
    async_fn_in_trait,
    reason = "the WASI body future stays local to its request task"
)]
pub trait BodyReader {
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, BodyReadError>;
}

/// All external authority behind the thin adapter.
///
/// Routing returns definitions, authentication applies the selected policy,
/// and invocation calls the typed operation through the host.
#[expect(
    async_fn_in_trait,
    reason = "the guest backend runs on the single-threaded component executor"
)]
pub trait Backend {
    type AuthenticatedCaller;

    fn routes(
        &mut self,
        method: &str,
        authority: &str,
    ) -> Result<Vec<RouteDefinition>, ProviderError>;
    async fn authenticate(
        &mut self,
        route: &RouteDefinition,
        headers: &[Header],
        body: &[u8],
    ) -> Result<Option<Self::AuthenticatedCaller>, AuthRejection>;
    fn validate_input(
        &mut self,
        route: &RouteDefinition,
        payload: &str,
    ) -> Result<(), SchemaInvalid>;
    async fn invoke(
        &mut self,
        route: &RouteDefinition,
        request: OperationRequest<Self::AuthenticatedCaller>,
    ) -> HttpResponse;
}

#[derive(Debug, Clone, PartialEq)]
struct MatchedRoute {
    definition: RouteDefinition,
}

/// Adapt one HTTP request without accessing graph or run-state storage.
pub async fn handle_request(
    backend: &mut impl Backend,
    body: &mut impl BodyReader,
    head: &RequestHead,
    limits: AdapterLimits,
) -> HttpResponse {
    try_handle(backend, body, head, limits)
        .await
        .unwrap_or_else(|response| response)
}

async fn try_handle(
    backend: &mut impl Backend,
    body: &mut impl BodyReader,
    head: &RequestHead,
    limits: AdapterLimits,
) -> Result<HttpResponse, HttpResponse> {
    if limits.body_bytes == 0 {
        return Err(error_response(500, "invalid-adapter-limits"));
    }
    let method =
        normalize_method(&head.method).ok_or_else(|| error_response(405, "method-not-allowed"))?;
    let authority = normalize_authority(&head.authority)
        .ok_or_else(|| error_response(400, "invalid-authority"))?;
    let (path, _query) =
        split_target(&head.target).ok_or_else(|| error_response(400, "invalid-target"))?;
    if method == "HEAD" && path == "/api/health" {
        return Ok(HttpResponse {
            status: 204,
            content_type: "application/json",
            body: Vec::new(),
            read: None,
        });
    }
    let routes = backend
        .routes(&method, &authority)
        .map_err(|_| error_response(503, "routing-provider-failed"))?;
    let mut matched =
        select_route(routes, &method, path).map_err(|code| error_response(404, code))?;
    let body_limit = usize::try_from(matched.definition.body_limit)
        .unwrap_or(usize::MAX)
        .min(limits.body_bytes);
    let raw_body = read_bounded(body, body_limit).await?;
    let caller = backend
        .authenticate(&matched.definition, &head.headers, &raw_body)
        .await
        .map_err(|rejection| rejection_response(&rejection))?;

    let body_json = if method == "GET" {
        // A read carries its one request element in the query string, in the one
        // canonical encoding (docs/architecture/execution.md).
        read_query_request_element(&head.target)?
    } else if raw_body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw_body).map_err(|_| error_response(400, "malformed-json"))?
    };
    let mapped = body_json;
    let payload = mapped.to_string();
    backend
        .validate_input(&matched.definition, &payload)
        .map_err(|refusal| {
            let data = refusal.pointer.map(|pointer| json!({ "pointer": pointer }));
            detailed_error_response(400, "schema-invalid", None, data)
        })?;
    // A request that carries the CSRF header is not cached
    // (docs/architecture/execution.md), and neither is any response but a success.
    let cache_control = matched
        .definition
        .cache_control
        .take()
        .filter(|_| header_values(&head.headers, CSRF_HEADER).is_empty())
        .map(|value| {
            if is_feed_read(&mapped) {
                feed_cache_control(&value)
            } else {
                value
            }
        });
    let trace = trace_context(&head.headers);
    let if_none_match = (method == "GET")
        .then(|| header_values(&head.headers, "if-none-match").join(", "))
        .filter(|value| !value.is_empty());
    let request = OperationRequest {
        payload,
        caller,
        trace,
        if_none_match,
    };
    let mut response = backend.invoke(&matched.definition, request).await;
    if matches!(response.status, 200 | 304) {
        let read = response.read.get_or_insert_with(|| {
            Box::new(ReadCache {
                cache_control: None,
                etag: None,
            })
        });
        read.cache_control = cache_control;
    } else {
        response.read = None;
    }
    Ok(response)
}

/// Whether a read carries the feed part of a list read, which reads the
/// changes since a cursor in place of a page (`docs/plan/wamn-sync.md`).
pub(crate) fn is_feed_read(input: &Value) -> bool {
    input
        .get(0)
        .and_then(|request_element| request_element.get("feed"))
        .is_some_and(Value::is_object)
}

/// The Cache-Control of a change feed read: the list's target, revalidated on
/// every read. Its weak tag on the cursor makes an unchanged feed a 304.
fn feed_cache_control(list: &str) -> String {
    let target = list.split(',').next().unwrap_or(list).trim();
    format!("{target}, no-cache")
}

/// The request header that repeats the readable CSRF cookie.
const CSRF_HEADER: &str = "x-wamn-csrf";

/// The longest request target a read accepts, path and query together.
const READ_TARGET_BYTES: usize = 8 * 1024;

/// The one request element of a read, as the list of one request element that the route
/// input schema describes.
fn read_query_request_element(target: &str) -> Result<Value, HttpResponse> {
    if target.len() > READ_TARGET_BYTES {
        return Err(error_response(400, "invalid-target"));
    }
    let query = target.split_once('?').map_or("", |(_, query)| query);
    let request_element = crate::read_query::decode_read_query(query)
        .map_err(|_| error_response(400, "invalid-target"))?;
    Ok(Value::Array(vec![Value::Object(request_element)]))
}

fn normalize_method(method: &str) -> Option<String> {
    let normalized = method.to_ascii_uppercase();
    (!normalized.is_empty()
        && normalized
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte == b'-'))
    .then_some(normalized)
}

fn normalize_authority(authority: &str) -> Option<String> {
    let normalized = authority.to_ascii_lowercase();
    (!normalized.is_empty()
        && !normalized.contains('/')
        && !normalized.chars().any(char::is_whitespace))
    .then_some(normalized)
}

fn split_target(target: &str) -> Option<(&str, &str)> {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    path.starts_with('/').then_some((path, query))
}
fn select_route(
    routes: Vec<RouteDefinition>,
    method: &str,
    path: &str,
) -> Result<MatchedRoute, &'static str> {
    routes
        .into_iter()
        .find(|route| route.method == method && route.path == path)
        .map(|definition| MatchedRoute { definition })
        .ok_or("route-not-found")
}

async fn read_bounded(body: &mut impl BodyReader, limit: usize) -> Result<Vec<u8>, HttpResponse> {
    let mut bytes = Vec::new();
    while let Some(chunk) = body
        .next_chunk()
        .await
        .map_err(|_| error_response(400, "body-read-failed"))?
    {
        let next = bytes
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| body_too_large_response(limit))?;
        if next > limit {
            return Err(body_too_large_response(limit));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn header_values(headers: &[Header], name: &str) -> Vec<String> {
    headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case(name))
        .map(|header| header.value.clone())
        .collect()
}

fn trace_context(headers: &[Header]) -> Option<TraceContext> {
    header_values(headers, "traceparent")
        .into_iter()
        .next()
        .map(|traceparent| TraceContext {
            traceparent,
            tracestate: header_values(headers, "tracestate").into_iter().next(),
        })
}

fn rejection_response(rejection: &AuthRejection) -> HttpResponse {
    let mut response = error_response(rejection.status, &rejection.code);
    if rejection.code.is_empty() {
        response.body.clear();
    }
    response
}

fn detailed_error_response(
    status: u16,
    code: &str,
    message: Option<&str>,
    data: Option<Value>,
) -> HttpResponse {
    let mut error = Map::from_iter([("code".to_string(), Value::String(code.to_string()))]);
    if let Some(message) = message {
        error.insert("message".to_string(), Value::String(message.to_string()));
    }
    if let Some(data) = data {
        error.insert("data".to_string(), data);
    }
    HttpResponse {
        read: None,
        status,
        content_type: "application/json",
        body: serde_json::to_vec(&ErrorEnvelope {
            error: Value::Object(error),
        })
        .unwrap_or_default(),
    }
}

fn error_response(status: u16, code: &str) -> HttpResponse {
    detailed_error_response(status, code, None, None)
}
fn body_too_large_response(limit: usize) -> HttpResponse {
    detailed_error_response(
        413,
        "body-too-large",
        Some("request body exceeds its limit"),
        Some(json!({"maximum": limit})),
    )
}

#[cfg(target_arch = "wasm32")]
mod guest;

#[cfg(any(target_arch = "wasm32", test))]
mod entry_response;

#[path = "../../../../../crates/catalog/model/src/read_query.rs"]
mod read_query;

#[allow(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen emits Vec::from_raw_parts with equal length and capacity"
)]
mod bindings;
