//! What a node invocation returned — the component→router event vocabulary.
//!
//! The router decides retry-vs-error-edge-vs-fail **mechanically from the
//! retained variant**, never by string-matching a message. This taxonomy is the
//! Rust mirror of the node-operation WIT package: it is a WIT-shaped enum
//! because it *is* the shape crossing that seam, and it is the only enum-shaped
//! error vocabulary in this crate (the router's own faults are contextual
//! structs — see [`ApplyError`](crate::ApplyError) and
//! [`WiringError`](crate::WiringError)).

use serde_json::Value;

/// The default output port a node emits on.
pub const MAIN_PORT: &str = "main";

/// The reserved error-path port.
pub const ERROR_PORT: &str = "error";

/// What an invoked node returned. `Success` carries the output payload and the
/// **port** it chose (a branching node selects a named port; most nodes emit on
/// [`MAIN_PORT`]); `Error` carries the classified failure.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeOutcome {
    Success {
        payload: Value,
        port: String,
        /// Whole-document in-memory context replacement. `None` leaves the
        /// current document unchanged; there are deliberately no merge
        /// semantics.
        context: Option<Value>,
    },
    Error(NodeError),
}

impl NodeOutcome {
    /// A success on the default `main` port — the common case.
    pub fn ok(payload: Value) -> NodeOutcome {
        NodeOutcome::Success {
            payload,
            port: MAIN_PORT.to_string(),
            context: None,
        }
    }

    /// A success routed out a named port (a branch).
    pub fn ok_on(payload: Value, port: impl Into<String>) -> NodeOutcome {
        NodeOutcome::Success {
            payload,
            port: port.into(),
            context: None,
        }
    }

    /// A success carrying a whole-document context replacement.
    pub fn ok_with_context(payload: Value, port: impl Into<String>, context: Value) -> NodeOutcome {
        NodeOutcome::Success {
            payload,
            port: port.into(),
            context: Some(context),
        }
    }
}

/// How a node classified its own failure. The router reads only the variant.
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
    /// An error detail containing only a message.
    pub fn msg(message: impl Into<String>) -> ErrorDetail {
        ErrorDetail {
            message: message.into(),
            code: None,
            data: None,
        }
    }

    /// An error detail with a stable machine-readable code.
    pub fn coded(code: impl Into<String>, message: impl Into<String>) -> ErrorDetail {
        ErrorDetail {
            message: message.into(),
            code: Some(code.into()),
            data: None,
        }
    }

    /// The payload sent down an error edge.
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
