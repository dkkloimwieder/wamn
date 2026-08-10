//! Pure execution and publication contract for the built-in standard nodes.
//!
//! This module deliberately contains no custom-node component identity,
//! recovery policy, or compatibility descriptors. Standard nodes are either
//! pure or effectful; an effectful occurrence is handled by the execution
//! plan's write-ahead effect protocol.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The default output port.
pub const MAIN_PORT: &str = "main";

/// The reserved error-path port.
pub const ERROR_PORT: &str = "error";

/// Whether a standard node can perform an external effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum EffectPolicy {
    Pure,
    Effectful,
}

/// A capability a standard node may declare and the runner may grant.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    HttpEgress,
    Postgres,
    RawSql,
}

/// One connection kind accepted by a standard node implementation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ConnectionRequirement {
    pub requirement_type: String,
    pub contract: String,
}

/// The complete environment-independent interface for a standard node type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct NodeInterface {
    pub node_type: String,
    pub output_ports: Vec<String>,
    pub capabilities: Vec<Capability>,
    pub connection_requirements: Vec<ConnectionRequirement>,
    pub effect_policy: EffectPolicy,
}

/// Everything a standard-node execution may need from its run.
#[derive(Debug, Clone, Copy)]
pub struct RunContext<'a> {
    pub run_id: &'a str,
    pub flow_id: &'a str,
    pub flow_version: u32,
    pub node_id: &'a str,
    pub connection: Option<&'a str>,
    pub attempt: u32,
    pub idempotency_key: &'a str,
    pub deadline_ms: Option<u64>,
    pub traceparent: Option<&'a str>,
    pub tracestate: Option<&'a str>,
    pub config: &'a Value,
    pub context: &'a Value,
}

impl RunContext<'_> {
    /// Return the W3C trace headers to propagate on an outbound request.
    pub fn trace_headers(&self) -> Vec<(String, String)> {
        let mut headers = Vec::new();
        if let Some(traceparent) = self.traceparent {
            headers.push(("traceparent".to_string(), traceparent.to_string()));
            if let Some(tracestate) = self.tracestate {
                headers.push(("tracestate".to_string(), tracestate.to_string()));
            }
        }
        headers
    }

    /// Add trace headers not already present, comparing names case-insensitively.
    pub fn apply_trace_context(&self, headers: &mut Vec<(String, String)>) {
        for (name, value) in self.trace_headers() {
            if !headers
                .iter()
                .any(|(header, _)| header.eq_ignore_ascii_case(&name))
            {
                headers.push((name, value));
            }
        }
    }
}

/// An outbound HTTP request made through the trusted effect adapter.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HttpRequest {
    pub requirement: String,
    pub method: String,
    pub path_and_query: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

/// The response to an [`HttpRequest`].
#[derive(Debug, Clone, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// A trusted HTTP capability failure before an HTTP status exists.
#[derive(Debug, Clone, PartialEq)]
pub enum HttpCapError {
    NotGranted,
    Denied,
    BadRequest(String),
    Transport(String),
}

/// A single bound Postgres parameter or result cell.
#[derive(Debug, Clone, PartialEq)]
pub enum PgValue {
    Null,
    Bool(bool),
    Int32(i32),
    Int64(i64),
    Float64(f64),
    Text(String),
    Bytes(Vec<u8>),
    Numeric(String),
    Timestamptz(String),
    Json(String),
    Uuid(String),
}

/// A Postgres query result.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PgRows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<PgValue>>,
}

/// A trusted Postgres capability failure.
#[derive(Debug, Clone, PartialEq)]
pub enum PgCapError {
    NotGranted,
    SerializationFailure,
    ConnectionUnavailable,
    StatementTimeout,
    RowLimitExceeded(u64),
    UniqueViolation(String),
    ForeignKeyViolation(String),
    CheckViolation(String),
    PermissionDenied,
    QueryError { code: String, message: String },
}

/// A credential-resolution failure.
#[derive(Debug, Clone, PartialEq)]
pub enum CredentialCapError {
    NotGranted,
    NotFound,
    Unavailable,
}

/// The runner-implemented capability surface used by standard nodes.
pub trait NodeCtx {
    fn http(&mut self, req: &HttpRequest) -> Result<HttpResponse, HttpCapError>;

    fn pg_query(&mut self, sql: &str, params: &[PgValue]) -> Result<PgRows, PgCapError>;

    fn pg_execute(&mut self, sql: &str, params: &[PgValue]) -> Result<u64, PgCapError>;

    fn catalog_json(&mut self) -> Result<String, PgCapError>;

    fn raw_sql_enabled(&self) -> bool {
        false
    }

    fn credential(&mut self) -> Result<String, CredentialCapError> {
        Err(CredentialCapError::NotGranted)
    }
}

/// A standard node's successful result.
#[derive(Debug, Clone, PartialEq)]
pub struct Emission {
    pub payload: Value,
    pub port: String,
    pub ctx: Option<Value>,
}

impl Emission {
    /// Create an emission on the default port.
    pub fn main(payload: Value) -> Self {
        Self {
            payload,
            port: MAIN_PORT.to_string(),
            ctx: None,
        }
    }

    /// Create an emission on a named port.
    pub fn on(payload: Value, port: impl Into<String>) -> Self {
        Self {
            payload,
            port: port.into(),
            ctx: None,
        }
    }

    /// Attach a whole-document run-context replacement.
    pub fn with_ctx(mut self, ctx: Value) -> Self {
        self.ctx = Some(ctx);
        self
    }
}

/// A built-in standard node implementation.
pub trait Node {
    fn capabilities(&self) -> &'static [Capability] {
        &[]
    }

    fn run(
        &self,
        ctx: &mut dyn NodeCtx,
        run: &RunContext<'_>,
        input: &Value,
    ) -> Result<Emission, NodeError>;
}

/// Classified standard-node failure.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeError {
    Retryable(ErrorDetail),
    RateLimited(RateLimitDetail),
    Terminal(ErrorDetail),
    InvalidInput(ErrorDetail),
    Cancelled,
}

/// Routing and display metadata carried by a node failure.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ErrorDetail {
    pub message: String,
    pub code: Option<String>,
    pub data: Option<Value>,
}

impl ErrorDetail {
    /// Create an error detail containing only a message.
    pub fn msg(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: None,
            data: None,
        }
    }

    /// Create an error detail with a stable machine-readable code.
    pub fn coded(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: Some(code.into()),
            data: None,
        }
    }

    /// Convert this detail into the payload sent down an error edge.
    pub fn to_error_payload(&self) -> Value {
        let mut error = serde_json::Map::new();
        error.insert("message".into(), Value::String(self.message.clone()));
        if let Some(code) = &self.code {
            error.insert("code".into(), Value::String(code.clone()));
        }
        if let Some(data) = &self.data {
            error.insert("data".into(), data.clone());
        }
        Value::Object(serde_json::Map::from_iter([(
            "error".to_string(),
            Value::Object(error),
        )]))
    }
}

/// A rate-limit failure with an optional source-authoritative delay.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RateLimitDetail {
    pub detail: ErrorDetail,
    pub retry_after_ms: Option<u64>,
    pub target_host: Option<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{EffectPolicy, Emission, NodeInterface};

    #[test]
    fn successful_emission_may_replace_context() {
        let emission = Emission::main(json!({"output": 1})).with_ctx(json!({"hold": {"id": 7}}));
        assert_eq!(emission.ctx, Some(json!({"hold": {"id": 7}})));
    }

    #[test]
    fn publication_policy_has_only_the_two_mvp_cases() {
        let policies = [EffectPolicy::Pure, EffectPolicy::Effectful];
        assert_eq!(policies.len(), 2);
        assert_eq!(
            serde_json::to_value(EffectPolicy::Effectful).expect("policy serializes"),
            json!("effectful")
        );

        let interface = NodeInterface {
            node_type: "transform".to_string(),
            output_ports: vec!["main".to_string()],
            capabilities: Vec::new(),
            connection_requirements: Vec::new(),
            effect_policy: EffectPolicy::Pure,
        };
        let value = serde_json::to_value(interface).expect("interface serializes");
        assert!(value.get("recovery").is_none());
    }
}
