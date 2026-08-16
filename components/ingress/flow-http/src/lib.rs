//! Thin HTTP adapter for the versioned callable-flow invocation contract.
//!
//! MVP outcome: M0 authenticated admission via the warm run-worker.

use boon::{Compiler, Draft, Schemas};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};
use wamn_flow_invocation::{BeginResult, InvokeRequest, InvokeResult, Rejection, TraceContext};

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
    pub catalog_version: u64,
    pub definition_hash: String,
    pub host: String,
    pub path: String,
    pub method: String,
    pub enabled: bool,
    pub auth_policy: String,
    pub mappings: Vec<Mapping>,
    pub input_schema: Value,
    pub idempotency_required: bool,
    pub body_limit: usize,
    pub mapped_limit: usize,
    pub deadline_override: Option<u64>,
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

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
struct ResponseWaitTimeout<'a> {
    code: &'static str,
    run_id: &'a str,
    retry: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
struct EffectUncertain<'a> {
    code: &'static str,
    run_id: &'a str,
}

/// The caller disconnected after admission, so there is no response to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterOutcome {
    Response(HttpResponse),
    Disconnected { run_id: String },
}

/// Adapter-owned finite waits and body ceilings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdapterLimits {
    pub body_bytes: usize,
    pub mapped_bytes: usize,
    pub wait_slice_ms: u32,
    pub max_waits: u32,
}

impl Default for AdapterLimits {
    fn default() -> Self {
        Self {
            body_bytes: 4 * 1024 * 1024,
            mapped_bytes: 4 * 1024 * 1024,
            wait_slice_ms: 250,
            max_waits: 120,
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

/// Incremental request-body source. A read error never produces a run.
pub trait BodyReader {
    fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, BodyReadError>;
}

/// Connection state observed between bounded provider waits.
pub trait ClientLiveness {
    fn connected(&mut self) -> bool;
}

/// All external authority behind the thin adapter.
///
/// Routing returns definitions, authentication applies the selected policy,
/// and invocation is exactly the frozen begin/wait protocol.
pub trait Backend {
    fn routes(
        &mut self,
        method: &str,
        authority: &str,
    ) -> Result<Vec<RouteDefinition>, ProviderError>;
    fn authenticate(&mut self, policy: &str, headers: &[Header]) -> Result<String, Rejection>;
    fn begin(&mut self, request: InvokeRequest) -> Result<BeginResult, ProviderError>;
    fn wait(
        &mut self,
        run_id: &str,
        timeout_ms: u32,
    ) -> Result<Option<InvokeResult>, ProviderError>;
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
    liveness: &mut impl ClientLiveness,
    head: &RequestHead,
    limits: AdapterLimits,
) -> AdapterOutcome {
    let result = try_handle(backend, body, liveness, head, limits);
    match result {
        Ok(outcome) => outcome,
        Err(response) => AdapterOutcome::Response(response),
    }
}

fn try_handle(
    backend: &mut impl Backend,
    body: &mut impl BodyReader,
    liveness: &mut impl ClientLiveness,
    head: &RequestHead,
    limits: AdapterLimits,
) -> Result<AdapterOutcome, HttpResponse> {
    if limits.body_bytes == 0
        || limits.mapped_bytes == 0
        || limits.wait_slice_ms == 0
        || limits.max_waits == 0
    {
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
    let principal = backend
        .authenticate(&matched.definition.auth_policy, &head.headers)
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

    let idempotency_key = header_values(&head.headers, "idempotency-key")
        .into_iter()
        .next();
    if matched.definition.idempotency_required && idempotency_key.is_none() {
        return Err(error_response(400, "idempotency-key-required"));
    }
    let trace = trace_context(&head.headers);
    let fingerprint = request_fingerprint(
        &method,
        &authority,
        &path,
        &query,
        header_values(&head.headers, "content-type")
            .first()
            .map(String::as_str),
        &raw_body,
    );
    let begin = backend
        .begin(InvokeRequest {
            attachment_id: matched.definition.attachment_id,
            expected_catalog_version: matched.definition.catalog_version,
            expected_definition_hash: matched.definition.definition_hash,
            client_request_fingerprint: fingerprint,
            payload,
            idempotency_key,
            principal,
            deadline_override: matched.definition.deadline_override,
            trace,
        })
        .map_err(|_| error_response(503, "invocation-provider-failed"))?;
    let admitted = match begin {
        BeginResult::Admitted(admitted) => admitted,
        BeginResult::Rejected(rejection) => return Err(rejection_response(rejection)),
    };

    for _ in 0..limits.max_waits {
        if !liveness.connected() {
            return Ok(AdapterOutcome::Disconnected {
                run_id: admitted.run_id,
            });
        }
        if let Some(result) = backend
            .wait(&admitted.run_id, limits.wait_slice_ms)
            .map_err(|_| error_response(503, "wait-provider-failed"))?
        {
            return Ok(AdapterOutcome::Response(invoke_response(result)));
        }
    }
    Err(response_wait_timeout(&admitted.run_id))
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
        .add_resource("mem://flow-input.json", schema.clone())
        .map_err(|_| ())?;
    let mut schemas = Schemas::new();
    let index = compiler
        .compile("mem://flow-input.json", &mut schemas)
        .map_err(|_| ())?;
    schemas.validate(value, index).map_err(|_| ())
}

fn request_fingerprint(
    method: &str,
    authority: &str,
    path: &str,
    query: &[(String, String)],
    content_type: Option<&str>,
    body: &[u8],
) -> String {
    let mut digest = Sha256::new();
    for value in [method, authority, path, content_type.unwrap_or("")] {
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value.as_bytes());
    }
    digest.update((query.len() as u64).to_be_bytes());
    for (name, value) in query {
        for item in [name, value] {
            digest.update((item.len() as u64).to_be_bytes());
            digest.update(item.as_bytes());
        }
    }
    digest.update(Sha256::digest(body));
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
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

fn rejection_response(rejection: Rejection) -> HttpResponse {
    error_response(rejection.status, &rejection.code)
}

fn invoke_response(result: InvokeResult) -> HttpResponse {
    match result {
        InvokeResult::Responded(response) => HttpResponse {
            status: response.status_hint.unwrap_or(200),
            body: response.body.into_bytes(),
        },
        InvokeResult::Failed(failure) if failure.error.code == "effect-uncertain" => {
            effect_uncertain_response(&failure.error.run_id)
        }
        InvokeResult::Failed(failure) => HttpResponse {
            status: failure.status,
            body: serde_json::to_vec(&json!({
                "error": {
                    "code": failure.error.code,
                    "message": failure.error.message,
                    "run-id": failure.error.run_id,
                    "flow-id": failure.error.flow_id,
                    "flow-version": failure.error.flow_version,
                }
            }))
            .unwrap_or_default(),
        },
    }
}

fn response_wait_timeout(run_id: &str) -> HttpResponse {
    HttpResponse {
        status: 504,
        body: serde_json::to_vec(&ErrorEnvelope {
            error: ResponseWaitTimeout {
                code: "response-wait-timeout",
                run_id,
                retry: "same-idempotency-key",
            },
        })
        .unwrap_or_default(),
    }
}

fn effect_uncertain_response(run_id: &str) -> HttpResponse {
    HttpResponse {
        status: 502,
        body: serde_json::to_vec(&ErrorEnvelope {
            error: EffectUncertain {
                code: "effect-uncertain",
                run_id,
            },
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
