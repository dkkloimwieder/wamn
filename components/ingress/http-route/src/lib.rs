//! Thin HTTP adapter from a released attachment to inline router delivery.

use boon::{Compiler, Draft, Schemas};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// A transport header after lowercasing its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub name: String,
    pub value: String,
}

/// Cheap request metadata available before reading the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestHead {
    pub method: String,
    pub authority: String,
    pub target: String,
    pub headers: Vec<Header>,
}

/// One route-selected transport mapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Mapping {
    pub from: MappingSource,
    pub name: String,
    pub to: String,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub cardinality: Cardinality,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MappingSource {
    Body,
    Path,
    Query,
    Header,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Cardinality {
    #[default]
    One,
    Many,
}

/// An authoritative route candidate returned by the routing provider.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteDefinition {
    pub attachment_id: String,
    pub host: String,
    pub path: String,
    pub method: String,
    pub mappings: Vec<Mapping>,
    pub input_schema: Value,
    pub body_limit: usize,
    pub mapped_limit: usize,
}

/// Authentication refusal returned by the selected route's policy owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthRejection {
    pub status: u16,
    pub code: String,
}

/// Trace context forwarded unchanged to component execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceContext {
    pub traceparent: String,
    pub tracestate: Option<String>,
}

/// One attachment-originated request to the host-owned router bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryRequest<Caller> {
    pub attachment_id: String,
    pub delivery_id: String,
    pub payload: String,
    pub caller: Option<Caller>,
    pub trace: Option<TraceContext>,
}

/// Stable router failure classes preserved by the delivery bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryFailureKind {
    Terminal,
    RetryExhausted,
    InvalidInput,
    HopLimit,
    UnreleasedCaller,
    MissingDedupId,
    RespondWithoutCaller,
    SecondVerdict,
}

/// A routed failure with no retired node or flow coordinate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryFailure {
    pub kind: DeliveryFailureKind,
    pub code: Option<String>,
    pub message: String,
}

/// A router emission returned to the HTTP boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Emission {
    pub event: String,
    pub dedup_id: String,
}

/// One terminal result from inline router execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryOutcome {
    Respond(String),
    Emit(Emission),
    Discard,
    Failed(DeliveryFailure),
    Cancelled,
}

/// Host-side delivery refusal before a router outcome exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryError {
    SourceNotFound,
    InvalidRequest,
    InvalidPayload,
    WiringNotPreloaded,
    ExecutionFailed,
    PermissionDenied { operation: String },
}

/// A bounded HTTP response produced by the adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

#[derive(Serialize)]
struct ErrorEnvelope<T> {
    error: T,
}

/// Adapter-owned body ceilings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdapterLimits {
    pub body_bytes: usize,
    pub mapped_bytes: usize,
}

impl Default for AdapterLimits {
    fn default() -> Self {
        Self {
            body_bytes: 4 * 1024 * 1024,
            mapped_bytes: 4 * 1024 * 1024,
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

/// Incremental request-body source. A read error never reaches the router.
pub trait BodyReader {
    fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, BodyReadError>;
}

/// All external authority behind the thin adapter.
///
/// Routing returns definitions, authentication applies the selected policy,
/// and delivery crosses the single host-owned router bridge.
pub trait Backend {
    type RoutePermit;
    type AuthenticatedCaller;

    fn routes(
        &mut self,
        method: &str,
        authority: &str,
    ) -> Result<Vec<RouteDefinition>, ProviderError>;
    fn authenticate(
        &mut self,
        attachment_id: &str,
        headers: &[Header],
    ) -> Result<Option<Self::AuthenticatedCaller>, AuthRejection>;
    fn try_acquire_route(
        &mut self,
        attachment_id: &str,
    ) -> Result<Option<Self::RoutePermit>, ProviderError>;
    fn new_delivery_id(&mut self) -> String;
    fn deliver(
        &mut self,
        request: DeliveryRequest<Self::AuthenticatedCaller>,
    ) -> Result<DeliveryOutcome, DeliveryError>;
}

#[derive(Debug, Clone, PartialEq)]
struct MatchedRoute {
    definition: RouteDefinition,
    path_values: Vec<(String, String)>,
}

/// Adapt one HTTP request without accessing graph or run-state storage.
pub fn handle_request(
    backend: &mut impl Backend,
    body: &mut impl BodyReader,
    head: &RequestHead,
    limits: AdapterLimits,
) -> HttpResponse {
    try_handle(backend, body, head, limits).unwrap_or_else(|response| response)
}

fn try_handle(
    backend: &mut impl Backend,
    body: &mut impl BodyReader,
    head: &RequestHead,
    limits: AdapterLimits,
) -> Result<HttpResponse, HttpResponse> {
    if limits.body_bytes == 0 || limits.mapped_bytes == 0 {
        return Err(error_response(500, "invalid-adapter-limits"));
    }
    let method =
        normalize_method(&head.method).ok_or_else(|| error_response(405, "method-not-allowed"))?;
    let authority = normalize_authority(&head.authority)
        .ok_or_else(|| error_response(400, "invalid-authority"))?;
    let (path, query) =
        split_target(&head.target).ok_or_else(|| error_response(400, "invalid-target"))?;
    let routes = backend
        .routes(&method, &authority)
        .map_err(|_| error_response(503, "routing-provider-failed"))?;
    let matched = select_route(routes, &method, &authority, &path)
        .map_err(|code| error_response(404, code))?;
    let caller = backend
        .authenticate(&matched.definition.attachment_id, &head.headers)
        .map_err(rejection_response)?;

    let body_limit = matched.definition.body_limit.min(limits.body_bytes);
    let raw_body = read_bounded(body, body_limit)?;
    let body_json = if raw_body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw_body).map_err(|_| error_response(400, "malformed-json"))?
    };
    let mapped = map_input(
        &matched.definition.mappings,
        body_json,
        &matched.path_values,
        &query,
        &head.headers,
    )?;
    validate_schema(&matched.definition.input_schema, &mapped)
        .map_err(|_| error_response(400, "schema-invalid"))?;
    let payload =
        serde_json::to_string(&mapped).map_err(|_| error_response(400, "mapping-failed"))?;
    let mapped_limit = matched.definition.mapped_limit.min(limits.mapped_bytes);
    if payload.len() > mapped_limit {
        return Err(error_response(413, "mapped-payload-too-large"));
    }

    let attachment_id = matched.definition.attachment_id;
    let Some(_permit) = backend
        .try_acquire_route(&attachment_id)
        .map_err(|_| error_response(503, "route-limit-provider-failed"))?
    else {
        return Err(error_response(429, "route-capacity-exhausted"));
    };
    let trace = trace_context(&head.headers);
    let delivery_id = backend.new_delivery_id();
    let outcome = backend
        .deliver(DeliveryRequest {
            attachment_id,
            delivery_id,
            payload,
            caller,
            trace,
        })
        .map_err(delivery_error_response)?;
    Ok(delivery_response(outcome))
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

fn split_target(target: &str) -> Option<(String, Vec<(String, String)>)> {
    let (path, raw_query) = target
        .split_once('?')
        .map_or((target, None), |(path, query)| (path, Some(query)));
    let path = normalize_request_path(path)?;
    let query = match raw_query {
        Some(query) => query
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| {
                let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
                Some((percent_decode(name)?, percent_decode(value)?))
            })
            .collect::<Option<Vec<_>>>()?,
        None => Vec::new(),
    };
    Some((path, query))
}

fn normalize_request_path(path: &str) -> Option<String> {
    if !path.starts_with('/') || path.contains("//") {
        return None;
    }
    let decoded = path
        .split('/')
        .skip(1)
        .map(percent_decode)
        .collect::<Option<Vec<_>>>()?;
    let normalized = decoded
        .iter()
        .map(|segment| percent_encode_segment(segment))
        .collect::<Vec<_>>()
        .join("/");
    let normalized = format!("/{}", normalized.trim_end_matches('/'));
    Some(if normalized.is_empty() {
        "/".to_string()
    } else {
        normalized
    })
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = *bytes.get(index + 1)?;
            let low = *bytes.get(index + 2)?;
            output.push(hex(high)? * 16 + hex(low)?);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output).ok()
}

fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn percent_encode_segment(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push_str(&format!("{byte:02X}"));
        }
    }
    encoded
}

fn select_route(
    routes: Vec<RouteDefinition>,
    method: &str,
    authority: &str,
    path: &str,
) -> Result<MatchedRoute, &'static str> {
    let mut candidates = routes
        .into_iter()
        .filter(|route| route.method.eq_ignore_ascii_case(method))
        .filter_map(|definition| {
            let host_score = if definition.host.eq_ignore_ascii_case(authority) {
                2
            } else if definition.host == "*" {
                1
            } else {
                return None;
            };
            let (path_score, path_values) = match_path(&definition.path, path)?;
            Some((host_score, path_score, definition, path_values))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        (right.0, &right.1, &right.2.attachment_id).cmp(&(left.0, &left.1, &left.2.attachment_id))
    });
    let Some((_, _, definition, path_values)) = candidates.into_iter().next() else {
        return Err("route-not-found");
    };
    Ok(MatchedRoute {
        definition,
        path_values,
    })
}

type NamedValues = Vec<(String, String)>;
type PathMatch = (Vec<u8>, NamedValues);

fn match_path(template: &str, path: &str) -> Option<PathMatch> {
    let template_segments = template.trim_matches('/').split('/').collect::<Vec<_>>();
    let path_segments = path
        .trim_matches('/')
        .split('/')
        .map(percent_decode)
        .collect::<Option<Vec<_>>>()?;
    let mut score = Vec::with_capacity(template_segments.len());
    let mut values = Vec::new();
    let mut path_index = 0;
    for segment in template_segments {
        if let Some(name) = segment
            .strip_prefix("{*")
            .and_then(|value| value.strip_suffix('}'))
        {
            score.push(0);
            values.push((name.to_string(), path_segments[path_index..].join("/")));
            path_index = path_segments.len();
            break;
        }
        let value = path_segments.get(path_index)?;
        if let Some(name) = segment
            .strip_prefix('{')
            .and_then(|value| value.strip_suffix('}'))
        {
            score.push(1);
            values.push((name.to_string(), value.clone()));
        } else if percent_decode(segment).as_deref() == Some(value.as_str()) {
            score.push(2);
        } else {
            return None;
        }
        path_index += 1;
    }
    (path_index == path_segments.len()).then_some((score, values))
}

fn read_bounded(body: &mut impl BodyReader, limit: usize) -> Result<Vec<u8>, HttpResponse> {
    let mut bytes = Vec::new();
    while let Some(chunk) = body
        .next_chunk()
        .map_err(|_| error_response(400, "body-read-failed"))?
    {
        let next = bytes
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| error_response(413, "payload-too-large"))?;
        if next > limit {
            return Err(error_response(413, "payload-too-large"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn map_input(
    mappings: &[Mapping],
    body: Value,
    path: &[(String, String)],
    query: &[(String, String)],
    headers: &[Header],
) -> Result<Value, HttpResponse> {
    if mappings.is_empty() {
        return Ok(body);
    }
    let mut output = Value::Object(Map::new());
    for source in [
        MappingSource::Body,
        MappingSource::Path,
        MappingSource::Query,
        MappingSource::Header,
    ] {
        for mapping in mappings.iter().filter(|mapping| mapping.from == source) {
            let values = mapping_values(mapping, &body, path, query, headers)?;
            let Some(value) = values else {
                if mapping.optional {
                    continue;
                }
                return Err(error_response(400, "mapping-source-required"));
            };
            set_pointer(&mut output, &mapping.to, value)?;
        }
    }
    Ok(output)
}

fn mapping_values(
    mapping: &Mapping,
    body: &Value,
    path: &[(String, String)],
    query: &[(String, String)],
    headers: &[Header],
) -> Result<Option<Value>, HttpResponse> {
    let values = match mapping.from {
        MappingSource::Body => {
            let value = if mapping.name.is_empty() {
                Some(body.clone())
            } else {
                body.as_object()
                    .and_then(|object| object.get(&mapping.name))
                    .cloned()
            };
            return Ok(value);
        }
        MappingSource::Path => named_values(path, &mapping.name),
        MappingSource::Query => named_values(query, &mapping.name),
        MappingSource::Header => header_values(headers, &mapping.name),
    };
    if values.is_empty() {
        return Ok(None);
    }
    match mapping.cardinality {
        Cardinality::One if values.len() == 1 => Ok(Some(Value::String(values[0].clone()))),
        Cardinality::One => Err(error_response(400, "mapping-cardinality")),
        Cardinality::Many => Ok(Some(Value::Array(
            values.into_iter().map(Value::String).collect(),
        ))),
    }
}

fn named_values(values: &[(String, String)], name: &str) -> Vec<String> {
    values
        .iter()
        .filter(|(candidate, _)| candidate == name)
        .map(|(_, value)| value.clone())
        .collect()
}

fn header_values(headers: &[Header], name: &str) -> Vec<String> {
    headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case(name))
        .map(|header| header.value.clone())
        .collect()
}

fn set_pointer(root: &mut Value, pointer: &str, value: Value) -> Result<(), HttpResponse> {
    if pointer.is_empty() {
        if !root.as_object().is_some_and(Map::is_empty) {
            return Err(error_response(400, "mapping-collision"));
        }
        *root = value;
        return Ok(());
    }
    let mut current = root;
    let mut tokens = pointer
        .strip_prefix('/')
        .ok_or_else(|| error_response(400, "mapping-pointer"))?
        .split('/')
        .peekable();
    while let Some(raw) = tokens.next() {
        let token = raw.replace("~1", "/").replace("~0", "~");
        let object = current
            .as_object_mut()
            .ok_or_else(|| error_response(400, "mapping-collision"))?;
        if tokens.peek().is_none() {
            if object.insert(token, value).is_some() {
                return Err(error_response(400, "mapping-collision"));
            }
            return Ok(());
        }
        current = object
            .entry(token)
            .or_insert_with(|| Value::Object(Map::new()));
    }
    Err(error_response(400, "mapping-pointer"))
}

fn validate_schema(schema: &Value, value: &Value) -> Result<(), ()> {
    let mut compiler = Compiler::new();
    compiler.set_default_draft(Draft::V2020_12);
    compiler
        .add_resource("mem://route-input.json", schema.clone())
        .map_err(|_| ())?;
    let mut schemas = Schemas::new();
    let index = compiler
        .compile("mem://route-input.json", &mut schemas)
        .map_err(|_| ())?;
    schemas.validate(value, index).map_err(|_| ())
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

fn rejection_response(rejection: AuthRejection) -> HttpResponse {
    error_response(rejection.status, &rejection.code)
}

fn delivery_response(outcome: DeliveryOutcome) -> HttpResponse {
    match outcome {
        DeliveryOutcome::Respond(payload) => HttpResponse {
            status: 200,
            body: payload.into_bytes(),
        },
        DeliveryOutcome::Emit(_) => error_response(500, "http-route-emitted"),
        DeliveryOutcome::Discard => error_response(500, "http-route-discarded"),
        DeliveryOutcome::Failed(failure) => {
            let status = if failure.kind == DeliveryFailureKind::InvalidInput {
                400
            } else {
                500
            };
            detailed_error_response(
                status,
                failure
                    .code
                    .as_deref()
                    .unwrap_or(failure_kind_code(failure.kind)),
                Some(&failure.message),
                None,
            )
        }
        DeliveryOutcome::Cancelled => error_response(503, "execution-cancelled"),
    }
}

fn delivery_error_response(error: DeliveryError) -> HttpResponse {
    let (status, code) = match error {
        DeliveryError::SourceNotFound => (404, "attachment-not-found"),
        DeliveryError::InvalidRequest => (400, "delivery-invalid-request"),
        DeliveryError::InvalidPayload => (400, "delivery-invalid-payload"),
        DeliveryError::WiringNotPreloaded => (503, "wiring-not-preloaded"),
        DeliveryError::ExecutionFailed => (503, "execution-failed"),
        DeliveryError::PermissionDenied { operation } => {
            return HttpResponse {
                status: 403,
                body: serde_json::to_vec(&json!({
                    "error": {
                        "code": "permission-denied",
                        "operation": operation,
                    }
                }))
                .unwrap_or_default(),
            };
        }
    };
    error_response(status, code)
}

fn failure_kind_code(kind: DeliveryFailureKind) -> &'static str {
    match kind {
        DeliveryFailureKind::Terminal => "terminal",
        DeliveryFailureKind::RetryExhausted => "retry-exhausted",
        DeliveryFailureKind::InvalidInput => "invalid-input",
        DeliveryFailureKind::HopLimit => "hop-limit",
        DeliveryFailureKind::UnreleasedCaller => "unreleased-caller",
        DeliveryFailureKind::MissingDedupId => "missing-dedup-id",
        DeliveryFailureKind::RespondWithoutCaller => "respond-without-caller",
        DeliveryFailureKind::SecondVerdict => "second-verdict",
    }
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
        status,
        body: serde_json::to_vec(&ErrorEnvelope {
            error: Value::Object(error),
        })
        .unwrap_or_default(),
    }
}

fn error_response(status: u16, code: &str) -> HttpResponse {
    HttpResponse {
        status,
        body: serde_json::to_vec(&json!({"error":{"code":code}})).unwrap_or_default(),
    }
}

#[cfg(target_arch = "wasm32")]
mod guest;
