//! Building the `record_receipt` envelope from what the operator entered.
//!
//! THE CLIENT SUPPLIES THREE THINGS the operator never types: `request_id`,
//! `value.idempotency_key`, and `value.occurred_at`. All three are passed in
//! rather than read from a clock or a random source here, so the builder stays
//! a pure function and a test can state them.
//!
//! # Why the idempotency key is the caller's, not the server's
//!
//! `record_receipt` is at-least-once: a request may be delivered twice, and
//! the second delivery must be the same receipt rather than a duplicate. That
//! only works if the key is decided BEFORE the first send and reused on every
//! retry of the same operator action — a key minted per attempt would make
//! every retry a new receipt, which is the exact failure the key exists to
//! prevent.

use serde_json::{Value, json};

use crate::model::AppState;
use crate::reduce::{entered_lines, submittable};

/// The three values the client contributes to one receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientSupplied {
    /// Correlates this envelope item with its outcome.
    pub request_id: String,
    /// Stable across every retry of the SAME operator action.
    pub idempotency_key: String,
    /// When the goods were received, RFC 3339.
    pub occurred_at: String,
}

/// Build the single-item `record_receipt` envelope.
///
/// # Errors
///
/// The reason the receipt is not ready to send, in the operator's terms.
pub fn record_receipt(
    state: &AppState,
    supplied: &ClientSupplied,
) -> Result<Vec<Value>, &'static str> {
    submittable(state)?;
    let order = state
        .receiving
        .as_ref()
        .ok_or("no purchase order is open")?;
    let location = state.picked_location().ok_or("a location is required")?;

    let mut lines = Vec::with_capacity(state.lines.len());
    for line in entered_lines(state) {
        // Sent as a JSON NUMBER, not a string: the contract declares
        // `numeric`, and a quoted quantity is a different wire value that the
        // input schema refuses.
        //
        // The digits are carried through serde_json's own number parsing
        // rather than `arbitrary_precision`. That feature would change how
        // EVERY number in the workspace serializes — the same class of global
        // hazard as `preserve_order`, which the repo already refuses — and
        // `float_roundtrip`, which the workspace does enable, guarantees a
        // parsed value re-serializes to the shortest string that reads back
        // identically. What the operator typed is what goes on the wire.
        let quantity: Value =
            serde_json::from_str(&line.entered).map_err(|_| "a quantity must be a number")?;
        if !quantity.is_number() {
            return Err("a quantity must be a number");
        }
        lines.push(json!({
            "purchase_order_line_id": line.purchase_order_line_id,
            "location_id": location.id,
            "quantity": quantity,
        }));
    }

    Ok(vec![json!({
        "request_id": supplied.request_id,
        "value": {
            "purchase_order_id": order.id,
            "receipt_reference": state.receipt_reference,
            "idempotency_key": supplied.idempotency_key,
            "occurred_at": supplied.occurred_at,
            "line": lines,
        },
    })])
}
