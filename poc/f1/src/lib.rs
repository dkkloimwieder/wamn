//! POC-F1 `receipt-received` node semantics — the PURE logic behind the
//! `poc-webhook-f1` component's F1-shaped nodes (`validate-receipt`,
//! `upsert-receipt`, `evaluate-specs`, `create-holds`, `respond`).
//!
//! The wamn-api split, applied to flow nodes: everything that does not need the
//! `wamn:postgres` binding or the wasi:http shell lives here — payload
//! validation (shape, required fields, the no-float rule), exact-decimal
//! arithmetic and spec evaluation, the parameterized SQL text the DB nodes
//! execute (values are ALWAYS `$n` params; identifiers are pinned to the
//! `poc-material-receiving` catalog's generated names), and the inter-node /
//! response JSON shapes (which must survive a JSON round trip: they are what
//! `node_runs.output_json` records and what 5.7 reconstruction replays).
//!
//! Deliberately F1-SCOPED: these are named, catalog-pinned nodes, not a generic
//! `postgres-query` node — the raw-SQL node lands with 5.3 under the D8
//! decision (wamn-r13: flag-gated raw-SQL node, default OFF; decision table).
//!
//! See `docs/poc-f1.md`; the flow graph itself is `deploy/poc/f1-flow.json`
//! (drift-guarded by this crate's tests).

mod decimal;
mod evaluate;
mod payload;
mod shapes;
pub mod sql;

pub use decimal::Decimal;
pub use evaluate::evaluate_line;
pub use payload::{Issue, Line, Receipt, parse_receipt};
pub use shapes::{
    EvalBranchOut, HoldEntry, LineSpec, OutOfSpec, UpsertOut, ValidateOut, ok_body, respond_status,
};

/// Node types the F1 flow uses and the `poc-webhook-f1` component implements.
/// `deploy/poc/f1-flow.json` must reference only these (drift-guarded in tests).
pub const NODE_TYPES: [&str; 5] = [
    "validate-receipt",
    "upsert-receipt",
    "evaluate-specs",
    "create-holds",
    "respond",
];
