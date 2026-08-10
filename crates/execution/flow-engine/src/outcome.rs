//! Node dispatch outcomes — the driver→engine event vocabulary.
//!
//! The error taxonomy ([`NodeError`] / [`ErrorDetail`] / [`RateLimitDetail`])
//! is defined in `wamn_flow::node_contract` and consumed directly here, so the
//! engine, the drivers, and every node crate share one definition while nodes
//! stay authorable without the runner (the 5.13 purity rule). It retains the
//! standard-node routing cases from the legacy `wamn:node` `node-error` WIT but
//! omits its custom-node-only cancellation case; the owning driver translates
//! that compatibility value once. The engine decides retry-vs-error-vs-fail
//! **mechanically from the retained variant** — never by string-matching a
//! message (`docs/archive/execution/wamn-node-design-notes.md` §6).

use serde_json::Value;
use wamn_flow::node_contract::NodeError;

/// The reserved error-path port (`wamn_flow::ERROR_PORT`).
pub use wamn_flow::ERROR_PORT;
/// The default output port a node emits on (`wamn_flow::MAIN_PORT`).
pub use wamn_flow::MAIN_PORT;

/// What a dispatched node returned. `Success` carries the output payload and the
/// **port** it chose (a branch node like `conditional` selects `"true"`/`"false"`;
/// most nodes emit on `MAIN_PORT`); `Error` carries the classified failure.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeOutcome {
    Success {
        payload: Value,
        port: String,
        /// Whole-document durable run-context replacement. `None` leaves the
        /// current document unchanged; there are deliberately no merge semantics.
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

    /// A success routed out a named port (branch).
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
