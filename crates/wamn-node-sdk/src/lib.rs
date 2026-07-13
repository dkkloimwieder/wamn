//! # wamn-node-sdk — the node authoring contract (5.3, ahead of the 5.4 freeze)
//!
//! The Rust mirror of the drafted `wamn:node` WIT contract
//! (`docs/wamn-node.wit`): the [`Node`] trait every standard-library node — and
//! later every custom node — is authored against, the [`RunContext`] view of a
//! dispatch, the [`NodeCtx`] capability facade all effects flow through, and
//! the [`NodeError`] taxonomy the engine folds mechanically.
//!
//! **This crate is the purity boundary** (docs/platform-plan.md 5.3/5.13):
//! node crates depend on the SDK ONLY — never on `wamn-runner` — enforced by a
//! dependency lint in `wamn-nodes`, so no node can circumvent the `wamn:node`
//! interface and silently break the frozen-flow composition path. `wamn-runner`
//! depends on this crate and re-exports the taxonomy, keeping one definition.
//!
//! Two deliberate deltas from the WIT draft, to reconcile at the 5.4 freeze:
//! - [`Emission`] carries an output **port** (the engine routes ported edges;
//!   a branch node like `conditional` selects `"true"`/`"false"`), which the
//!   drafted `run` result does not yet express.
//! - Payloads are in-memory [`serde_json::Value`]s; the `streamed(payload-ref)`
//!   arm waits for the payload store (5.10).

mod ctx;
mod error;

pub use ctx::{
    Capability, HttpCapError, HttpRequest, HttpResponse, NodeCtx, PgCapError, PgRows, PgValue,
    RunContext,
};
pub use error::{ErrorDetail, NodeError, RateLimitDetail};

use serde_json::Value;

/// The default output port a node emits on. Mirrors `wamn_flow::MAIN_PORT`
/// (drift-guarded by a `wamn-runner` test; the SDK must not depend on the flow
/// schema crate).
pub const MAIN_PORT: &str = "main";
/// The reserved error-path port. Mirrors `wamn_flow::ERROR_PORT`.
pub const ERROR_PORT: &str = "error";

/// A node's successful result: the output payload and the port it emits on.
#[derive(Debug, Clone, PartialEq)]
pub struct Emission {
    pub payload: Value,
    pub port: String,
}

impl Emission {
    /// An emission on the default `main` port — the common case.
    pub fn main(payload: Value) -> Emission {
        Emission {
            payload,
            port: MAIN_PORT.to_string(),
        }
    }

    /// An emission routed out a named port (branch).
    pub fn on(payload: Value, port: impl Into<String>) -> Emission {
        Emission {
            payload,
            port: port.into(),
        }
    }
}

/// A node implementation: a pure function of (ctx, run-context, input) — every
/// effect goes through the granted [`NodeCtx`] capabilities. That purity is
/// what makes nodes unit-testable against a mock ctx and frozen-flow
/// composition possible (`docs/wamn-node.wit`).
pub trait Node {
    /// The capabilities this node type needs — its row in the dispatch-time
    /// policy table. The runner refuses the dispatch if it cannot grant them
    /// all, and the gated ctx refuses calls outside this set.
    fn capabilities(&self) -> &'static [Capability] {
        &[]
    }

    /// Execute the node over `input`, using only declared capabilities.
    fn run(
        &self,
        ctx: &mut dyn NodeCtx,
        run: &RunContext<'_>,
        input: &Value,
    ) -> Result<Emission, NodeError>;
}
