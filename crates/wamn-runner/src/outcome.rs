//! Node dispatch outcomes — the driver→engine event vocabulary.
//!
//! [`NodeError`] is a 1:1 **mirror** of the `wamn:node` `node-error` WIT variant
//! (`docs/wamn-node.wit`), the same way `wamn_api::SqlValue` mirrors the
//! `wamn:postgres` `sql-value`: the component maps the WIT binding to these types
//! with a trivial `match`, and the engine decides retry-vs-error-vs-fail
//! **mechanically from the variant** — never by string-matching a message
//! (`docs/wamn-node-design-notes.md` §6). The one runner-side addition is
//! [`RateLimitDetail::target_host`], which the driver fills from the node's target
//! so the engine can key the shared throttle by `(node type, credential, host)`.

use serde_json::Value;

/// The reserved error-path port (`wamn_flow::ERROR_PORT`).
pub use wamn_flow::ERROR_PORT;
/// The default output port a node emits on (`wamn_flow::MAIN_PORT`).
pub use wamn_flow::MAIN_PORT;

/// What a dispatched node returned. `Success` carries the output payload and the
/// **port** it chose (a branch node like `conditional` selects `"true"`/`"false"`;
/// most nodes emit on `MAIN_PORT`); `Error` carries the classified failure.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeOutcome {
    Success { payload: Value, port: String },
    Error(NodeError),
}

impl NodeOutcome {
    /// A success on the default `main` port — the common case.
    pub fn ok(payload: Value) -> NodeOutcome {
        NodeOutcome::Success {
            payload,
            port: MAIN_PORT.to_string(),
        }
    }

    /// A success routed out a named port (branch).
    pub fn ok_on(payload: Value, port: impl Into<String>) -> NodeOutcome {
        NodeOutcome::Success {
            payload,
            port: port.into(),
        }
    }
}

/// Classified node failure — mirrors `wamn:node`'s `node-error`. The engine's
/// action for each variant is fixed (see [`crate::engine`]):
///
/// | variant        | engine action |
/// |----------------|---------------|
/// | `Retryable`    | retry per the node's [`RetryPolicy`](crate::RetryPolicy), then error-path/fail |
/// | `RateLimited`  | retry honoring the source delay + engage the shared throttle |
/// | `Terminal`     | route to the flow's error path immediately (no retry) |
/// | `InvalidInput` | never retried; distinct terminal reason in run history |
/// | `Cancelled`    | run recorded `cancelled`, error branches do not fire |
#[derive(Debug, Clone, PartialEq)]
pub enum NodeError {
    /// Transient; the runner may retry per the node's retry policy.
    Retryable(ErrorDetail),
    /// The upstream signaled throttling: retryable with a source-authoritative
    /// delay and a **shared** runner throttle keyed by (node type, credential,
    /// target host) so parallel executions against one limited system back off
    /// together instead of stampeding.
    RateLimited(RateLimitDetail),
    /// Permanent; the runner routes to the flow's error path immediately.
    Terminal(ErrorDetail),
    /// Input contract violated; never retried, flagged distinctly in run history
    /// (usually an upstream bug — does not burn retry budget).
    InvalidInput(ErrorDetail),
    /// The node observed a cancellation request and stopped cooperatively.
    Cancelled,
}

/// Routing / display metadata carried by a failure. Mirrors `wamn:node`'s
/// `error-detail`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ErrorDetail {
    pub message: String,
    /// Machine-readable code for editor display / error-branch labeling, e.g.
    /// `"ECONNREFUSED"`, `"HTTP_429"`.
    pub code: Option<String>,
    /// Optional structured payload surfaced in run history.
    pub data: Option<Value>,
}

impl ErrorDetail {
    /// A detail with just a message.
    pub fn msg(message: impl Into<String>) -> ErrorDetail {
        ErrorDetail {
            message: message.into(),
            code: None,
            data: None,
        }
    }

    /// The JSON the engine hands to an error-path node: `{"error": {...}}`.
    pub(crate) fn to_error_payload(&self) -> Value {
        let mut err = serde_json::Map::new();
        err.insert("message".into(), Value::String(self.message.clone()));
        if let Some(code) = &self.code {
            err.insert("code".into(), Value::String(code.clone()));
        }
        if let Some(data) = &self.data {
            err.insert("data".into(), data.clone());
        }
        Value::Object(serde_json::Map::from_iter([(
            "error".to_string(),
            Value::Object(err),
        )]))
    }
}

/// A rate-limit failure. `retry_after_ms` is the source-authoritative delay (e.g.
/// from a `Retry-After` header); `None` means the runner applies its own backoff
/// curve. `target_host` is runner-side metadata (not in the WIT) the driver fills
/// from the node's target so the shared throttle key is complete.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RateLimitDetail {
    pub detail: ErrorDetail,
    pub retry_after_ms: Option<u64>,
    pub target_host: Option<String>,
}
