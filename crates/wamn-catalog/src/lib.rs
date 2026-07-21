//! Canonical wamn metadata catalog schema (3.1).
//!
//! The catalog is **data, not DDL**: a versioned set of entities ([`Entity`]),
//! each with typed fields ([`Field`] / [`FieldType`]) plus indexes ([`Index`])
//! and constraints ([`Constraint`]), wired by relations ([`Relation`]). It is
//! the model the DDL compiler (3.2) turns into migrations, the generated API
//! (4.1) exposes as CRUD, the designer UI (3.3) edits, and the RLS builder (3.5)
//! attaches policies to. The core stays **neutral** (D14) — opinionated domain
//! models live in optional modules.
//!
//! This crate provides:
//!
//! - **types** — the canonical serde model ([`Catalog`] and friends);
//! - **import/export** — [`Catalog::from_json`] / [`Catalog::to_json`]
//!   (round-trip; the 3.4 promotion format);
//! - **validation** — [`Catalog::validate`] (structural well-formedness incl.
//!   the exact-decimal / no-float rule and the system-entity extension rule);
//! - **diff** — [`diff::diff`] (structured version diff feeding the 3.2 DDL
//!   compiler and 11.8 schema-impact analysis);
//! - **contract** — [`json_schema`] generates the language-neutral JSON Schema
//!   published at `docs/catalog-model.schema.json` (drift-guarded by a test).

mod diff;
mod types;
mod validate;

pub use diff::{CatalogDiff, EntityChange, FieldChange, diff};
pub use types::{
    Cardinality, Catalog, Constraint, Entity, EntityId, Field, FieldId, FieldType, Index, Relation,
    SCHEMA_VERSION,
};
pub use validate::{
    Issue, MAX_IDENTIFIER_BYTES, Severity, SynthesizedIdentifiers, synthesized_identifiers,
    unsafe_expression_reason, validate,
};

/// The JSON Schema for [`Catalog`], generated from the Rust types (the single
/// source of truth). Serialized to `docs/catalog-model.schema.json`; a drift
/// test keeps the committed file in lockstep with the types.
pub fn json_schema() -> serde_json::Value {
    let schema = schemars::schema_for!(Catalog);
    serde_json::to_value(schema).expect("schema serializes")
}

/// [`json_schema`] as canonical pretty JSON with a trailing newline — the exact
/// bytes of the committed contract file.
pub fn json_schema_string() -> String {
    let mut s = serde_json::to_string_pretty(&json_schema()).expect("schema serializes");
    s.push('\n');
    s
}
