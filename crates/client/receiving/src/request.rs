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
        // Sent as a JSON STRING carrying the typed digits verbatim, and SCALE
        // IS THE REASON. The contract declares `numeric` under the
        // canonicalization `postgresql_lexical_scale_preserved`; a JSON number
        // cannot carry `5.0000`, because every reader re-serializes it as
        // `5.0` and the scale the operator typed is gone. The published route
        // agrees — `value.line[].quantity` is `{"type": "string"}` in the
        // wiring's input schema — so a number is refused at ingress with
        // `{"error":{"code":"schema-invalid"}}` before the operation runs, and
        // no receipt is ever recorded. Measured live against the served
        // release; the recipe is `[RECEIVING-TUI]`.
        //
        // The reducer already refuses anything but digits and at most one
        // decimal point at entry, so the only shape left to reject is a lone
        // separator.
        let quantity = line.entered.trim();
        if quantity.chars().all(|symbol| !symbol.is_ascii_digit()) {
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
