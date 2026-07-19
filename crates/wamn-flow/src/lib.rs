//! Canonical wamn flow-graph schema (5.1).
//!
//! A flow is **data, not code**: a versioned directed graph of typed nodes
//! ([`Node`]) wired by ported edges ([`Edge`]), invoked by one [`Trigger`],
//! referencing credentials by name ([`CredentialRef`]). Deploying a flow flips
//! an active-version pointer (5.14); the graph itself is this crate's [`Flow`].
//!
//! This crate is the shared foundation the production runner (5.2) and the
//! editor build on. It provides:
//!
//! - **types** — the canonical serde model ([`Flow`] and friends);
//! - **import/export** — [`Flow::from_json`] / [`Flow::to_json`] (round-trip);
//! - **validation** — [`Flow::validate`] (graph well-formedness; per-node-type
//!   `config` is validated by the node library, 5.3, not here);
//! - **diff** — [`diff::diff`] (structured version diff for the editor);
//! - **contract** — [`json_schema`] generates the language-neutral JSON Schema
//!   published at `docs/flow-schema.schema.json` (drift-guarded by a test).

mod diff;
mod types;
mod validate;

pub use diff::{FlowDiff, NodeChange, diff};
pub use types::{
    CredentialRef, ERROR_PORT, Edge, Flow, MAIN_PORT, Node, NodeId, Ordering, PartitionPolicy,
    RowEvent, SCHEMA_VERSION, Trigger,
};
pub use validate::{Issue, Severity, validate};

/// The JSON Schema for [`Flow`], generated from the Rust types (the single
/// source of truth). Serialized to `docs/flow-schema.schema.json`; a drift test
/// keeps the committed file in lockstep with the types.
pub fn json_schema() -> serde_json::Value {
    let schema = schemars::schema_for!(Flow);
    serde_json::to_value(schema).expect("schema serializes")
}

/// [`json_schema`] as canonical pretty JSON with a trailing newline — the exact
/// bytes of the committed contract file.
pub fn json_schema_string() -> String {
    let mut s = serde_json::to_string_pretty(&json_schema()).expect("schema serializes");
    s.push('\n');
    s
}
