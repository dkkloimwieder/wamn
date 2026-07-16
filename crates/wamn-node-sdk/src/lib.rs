//! # wamn-node-sdk — the node authoring contract (5.3/5.4)
//!
//! The Rust mirror of the FROZEN `wamn:node` 0.1 WIT contract
//! (`docs/wamn-node.wit`, frozen by 5.4): the [`Node`] trait every
//! standard-library node — and every custom node, via the
//! `wamn-node-guest` scaffolding — is authored against, the [`RunContext`]
//! view of a dispatch, the [`NodeCtx`] capability facade all effects flow
//! through, and the [`NodeError`] taxonomy the engine folds mechanically.
//! `crates/wamn-node-sdk/tests/wit_coherence.rs` drift-guards the mirror
//! against the WIT file and every vendored copy of it.
//!
//! **This crate is the purity boundary** (docs/platform-plan.md 5.3/5.13):
//! node crates depend on the SDK ONLY — never on `wamn-runner` — enforced by a
//! dependency lint in `wamn-nodes`, so no node can circumvent the `wamn:node`
//! interface and silently break the frozen-flow composition path. `wamn-runner`
//! depends on this crate and re-exports the taxonomy, keeping one definition.
//!
//! One deliberate delta from the frozen WIT remains: payloads are in-memory
//! [`serde_json::Value`]s — the `streamed(payload-ref)` arm waits for the
//! payload store (5.10; the scaffolding refuses a streamed input with
//! `terminal("streamed-payload-unsupported")` until then). [`Emission::port`]
//! `== MAIN_PORT` corresponds to an ABSENT port in the WIT emission record.

mod ctx;
mod error;

pub use ctx::{
    Capability, CredentialCapError, HttpCapError, HttpRequest, HttpResponse, NodeCtx, PgCapError,
    PgRows, PgValue, RunContext,
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
