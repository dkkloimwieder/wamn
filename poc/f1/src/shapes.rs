//! Inter-node payload and response shapes. These are what the engine threads
//! between nodes, what `node_runs.output_json` records (so they must survive a
//! JSON round trip for 5.7 reconstruction), and — for [`ok_body`] — the sync
//! response contract: `{receipt_id, holds: [...]}`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::payload::Receipt;

/// A line's material resolution: the FK the upsert writes plus the two spec
/// values evaluate compares against. Index-aligned with `Receipt::lines`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineSpec {
    pub material_id: String,
    pub moisture_max_pct: String,
    pub weight_tolerance_kg: String,
}

/// `validate-receipt` output: the validated payload plus every business key
/// resolved to its uuid (unknown keys were invalid-input).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidateOut {
    pub receipt: Receipt,
    pub supplier_id: String,
    pub site_id: String,
    pub line_specs: Vec<LineSpec>,
}

/// `upsert-receipt` output: validate's payload plus the persisted ids —
/// `receipt_id` from the upsert's RETURNING, `line_ids` index-aligned with
/// `receipt.lines`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpsertOut {
    pub receipt: Receipt,
    pub supplier_id: String,
    pub site_id: String,
    pub line_specs: Vec<LineSpec>,
    pub receipt_id: String,
    pub line_ids: Vec<String>,
}

/// One out-of-spec line, as evaluate emits it on the `out-of-spec` port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutOfSpec {
    /// 1-based position in the posted `lines` array.
    pub line: u32,
    pub line_id: String,
    pub material: String,
    pub reason: String,
}

/// `evaluate-specs` branch output (the `out-of-spec` port; the in-spec main
/// port emits the final `{receipt_id, holds: []}` body directly).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalBranchOut {
    pub receipt_id: String,
    pub site_id: String,
    pub out_of_spec: Vec<OutOfSpec>,
}

/// One hold in the sync response: the persisted `quality_holds` row's id plus
/// the evaluation context a caller needs to act on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoldEntry {
    pub hold_id: String,
    /// 1-based position in the posted `lines` array.
    pub line: u32,
    pub material: String,
    pub reason: String,
    /// Always `"open"` — the `quality_holds.status` the row was created with.
    pub status: String,
}

/// The sync response body: `{receipt_id, holds: [...]}` (the F1 acceptance
/// contract). In-spec receipts carry an empty `holds`.
pub fn ok_body(receipt_id: &str, holds: &[HoldEntry]) -> Value {
    serde_json::json!({ "receipt_id": receipt_id, "holds": holds })
}

/// The `respond` node's HTTP status decision. The configured `status` (default
/// 200) answers — EXCEPT on an error-path respond (`config.error` set) whose
/// incoming payload carries a different `error.code`: that is an
/// infrastructure failure routed down the error edge, not the client fault the
/// node was configured for, and it answers 503 instead of the configured 4xx.
pub fn respond_status(config: &Value, payload: &Value) -> u16 {
    let configured = config
        .get("status")
        .and_then(Value::as_u64)
        .and_then(|s| u16::try_from(s).ok())
        .unwrap_or(200);
    match config.get("error").and_then(Value::as_str) {
        Some(expected)
            if payload.pointer("/error/code").and_then(Value::as_str) != Some(expected) =>
        {
            503
        }
        _ => configured,
    }
}

macro_rules! value_round_trip {
    ($t:ty) => {
        impl $t {
            /// Serialize for the engine payload / `node_runs` record.
            pub fn to_value(&self) -> Value {
                serde_json::to_value(self).unwrap_or(Value::Null)
            }

            /// Parse the upstream node's payload back out of the engine.
            pub fn from_value(v: &Value) -> Result<Self, String> {
                serde_json::from_value(v.clone())
                    .map_err(|e| format!(concat!(stringify!($t), " payload: {}"), e))
            }
        }
    };
}

value_round_trip!(ValidateOut);
value_round_trip!(UpsertOut);
value_round_trip!(EvalBranchOut);
