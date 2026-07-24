//! Node dispatch outcomes — the driver→engine event vocabulary.
//!
//! The error taxonomy ([`NodeError`] / [`ErrorDetail`] / [`RateLimitDetail`])
//! is DEFINED in `wamn-node-sdk` (the node authoring contract, 5.3) and
//! re-exported here, so the engine, the drivers, and every node crate share
//! one definition while nodes stay authorable without the runner (the 5.13
//! purity rule). It is a 1:1 mirror of the `wamn:node` `node-error` WIT
//! variant (`docs/wamn-node.wit`); the engine decides retry-vs-error-vs-fail
//! **mechanically from the variant** — never by string-matching a message
//! (`docs/wamn-node-design-notes.md` §6).

use serde_json::Value;

/// The reserved error-path port (`wamn_flow::ERROR_PORT`).
pub use wamn_flow::ERROR_PORT;
/// The default output port a node emits on (`wamn_flow::MAIN_PORT`).
pub use wamn_flow::MAIN_PORT;

pub use wamn_node_sdk::{ErrorDetail, NodeError, RateLimitDetail};

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
