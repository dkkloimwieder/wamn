//! Node emission, failure, and connection-requirement vocabulary.
//!
//! The built-in standard nodes this module was written for are gone: the node
//! library went with wamn-0h0g.26.4 and the registry that declared them — the
//! `node-type` interface, its effect and capability classification, and the
//! `Node`/`NodeCtx` traits the runner drove — went with wamn-0h0g.26.14. What
//! survives is the vocabulary other bounded contexts still speak beside the
//! `wamn:node` seam the router walks: the emission and failure shapes, the
//! connection requirement a node names, and portable HTTP target normalization.

#[path = "portable_http_target.rs"]
mod portable_http_target;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[doc(inline)]
pub use portable_http_target::{
    CanonicalHttpTarget, PortableHttpTargetError, normalize_portable_http_target,
};

/// One connection kind a node names and its environment binding satisfies.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ConnectionRequirement {
    pub requirement_type: String,
    pub contract: String,
}

/// A node's successful result.
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
            port: crate::MAIN_PORT.to_string(),
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

/// Classified node failure.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeError {
    Retryable(ErrorDetail),
    RateLimited(RateLimitDetail),
    Terminal(ErrorDetail),
    InvalidInput(ErrorDetail),
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

    use super::Emission;

    #[test]
    fn successful_emission_may_replace_context() {
        let emission = Emission::main(json!({"output": 1})).with_ctx(json!({"hold": {"id": 7}}));
        assert_eq!(emission.ctx, Some(json!({"hold": {"id": 7}})));
    }
}
