//! Canonical wamn flow-graph schema (5.1).
//!
//! A flow is **data, not code**: a versioned directed graph of typed nodes
//! ([`Node`]) wired by ported edges ([`Edge`]), starting at one typed entry,
//! referencing credentials by name ([`CredentialRef`]). Deploying a flow flips
//! an active-version pointer (5.14); the graph itself is this crate's [`Flow`].
//!
//! This crate is the shared foundation the production runner (5.2) and the
//! editor build on. It provides:
//!
//! - **types** — the canonical serde model ([`Flow`] and friends);
//! - **import/export** — [`Flow::from_json`] / [`Flow::to_json`] (round-trip);
//! - **validation** — [`Flow::validate`] (graph well-formedness against pinned
//!   node interfaces; ordinary node `config` remains node-library-owned);
//! - **diff** — [`diff::diff`] (structured version diff for the editor);
//! - **contract** — [`json_schema`] generates the language-neutral JSON Schema
//!   published at `docs/flow-schema.schema.json` (drift-guarded by a test).

mod canonical;
mod diff;
mod types;
mod validate;

use std::fmt::Write as _;

use serde_json::Value;

pub use diff::{FlowDiff, NodeChange, diff};
pub use types::{
    Capture, CaptureMode, CredentialRef, CronInput, DEFAULT_CAPTURE_MAX_BYTES, ENTRY_TYPES,
    ERROR_PORT, Edge, EntryKind, EventInput, FailConfig, Flow, MAIN_PORT, Node, NodeId, Ordering,
    PartitionPolicy, RequestConfig, RespondConfig, RowEvent, SCHEMA_VERSION,
};
pub use validate::{Issue, ResolvedInterfaces, Severity, validate};

/// `sha256:<hex>` over the RFC 8785 canonical representation of arbitrary JSON.
///
/// This is the stable identity used when a durable boundary must compare a
/// replayed JSON body with the outcome that previously committed.
pub fn canonical_json_sha256(value: &Value) -> String {
    let digest = canonical::sha256(&canonical_json_bytes(value));
    let mut hash = String::with_capacity("sha256:".len() + digest.len() * 2);
    hash.push_str("sha256:");
    for byte in digest {
        write!(&mut hash, "{byte:02x}").expect("writing to String cannot fail");
    }
    hash
}

/// RFC 8785 canonical bytes for an arbitrary JSON value.
///
/// Durable boundaries that verify a hash outside Rust must bind these exact
/// bytes rather than a normal JSON serialization or PostgreSQL `jsonb::text`.
pub fn canonical_json_bytes(value: &Value) -> Vec<u8> {
    canonical::to_vec(value)
}

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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{canonical_json_bytes, canonical_json_sha256};

    #[test]
    fn canonical_json_hash_ignores_object_insertion_order() {
        let left = canonical_json_sha256(&json!({"a": 1, "b": 2}));
        let reordered = canonical_json_sha256(&json!({"b": 2, "a": 1}));
        let changed = canonical_json_sha256(&json!({"a": 9, "b": 2}));

        assert_eq!(left, reordered);
        assert_ne!(left, changed);
        assert!(left.starts_with("sha256:"));
        assert_eq!(left.len(), "sha256:".len() + 64);
    }

    #[test]
    fn durable_identity_bytes_cover_ordering_and_numeric_edges() {
        let left = json!({"z": -0.0, "large": 1e30, "small": 0.000001, "nested": {"b": 2, "a": 1}});
        let reordered =
            json!({"nested": {"a": 1, "b": 2}, "small": 0.000001, "large": 1e30, "z": -0.0});
        let bytes = canonical_json_bytes(&left);

        assert_eq!(bytes, canonical_json_bytes(&reordered));
        assert_eq!(
            canonical_json_sha256(&left),
            canonical_json_sha256(&reordered)
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            serde_json::from_slice::<serde_json::Value>(&canonical_json_bytes(&reordered)).unwrap()
        );
    }
}
