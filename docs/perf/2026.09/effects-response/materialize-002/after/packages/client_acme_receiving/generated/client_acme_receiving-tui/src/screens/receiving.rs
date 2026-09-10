// @generated; do not edit.

use wamn_client_tui::{screen, submission};

pub static RECORD_RECEIPT_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "receiving",
    name: "record_receipt",
    operation: "client-acme-receiving:receiving/record-receipt@3.0.0",
    kind: "command",
    input: crate::receiving::RECEIVING_RECORD_RECEIPT_INPUT_SCHEMA,
    input_schema: Some("{\"items\":{\"additionalProperties\":false,\"properties\":{\"request_id\":{\"minLength\":1,\"type\":\"string\"},\"value\":{\"additionalProperties\":false,\"properties\":{\"idempotency_key\":{\"minLength\":1,\"type\":\"string\"},\"line\":{\"items\":{\"additionalProperties\":false,\"properties\":{\"location_id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"},\"purchase_order_line_id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"},\"quantity\":{\"type\":\"string\"}},\"required\":[\"purchase_order_line_id\",\"quantity\",\"location_id\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"},\"occurred_at\":{\"type\":\"string\"},\"purchase_order_id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"},\"receipt_reference\":{\"minLength\":1,\"type\":\"string\"}},\"required\":[\"idempotency_key\",\"purchase_order_id\",\"receipt_reference\",\"occurred_at\",\"line\"],\"type\":\"object\"}},\"required\":[\"request_id\",\"value\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}"),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::receiving::RECEIVING_RECORD_RECEIPT_RESULT_SCHEMA,
        result_class: Some("one"),
        errors: &[
            submission::ErrorCase { literal: "idempotency_conflict", required: &["field"], sources: &["transaction_invariant"] },
            submission::ErrorCase { literal: "internal_error", required: &[], sources: &["query_error", "row_limit_exceeded", "undeclared_constraint"] },
            submission::ErrorCase { literal: "invalid_input", required: &["field"], sources: &["envelope_count", "line_count", "malformed_input"] },
            submission::ErrorCase { literal: "location_not_found", required: &["field", "id"], sources: &["transaction_invariant"] },
            submission::ErrorCase { literal: "permission_denied", required: &["operation"], sources: &["permission_denied"] },
            submission::ErrorCase { literal: "purchase_order_line_mismatch", required: &["field", "id"], sources: &["transaction_invariant"] },
            submission::ErrorCase { literal: "purchase_order_line_not_found", required: &["field", "id"], sources: &["transaction_invariant"] },
            submission::ErrorCase { literal: "purchase_order_not_found", required: &["field"], sources: &["transaction_invariant"] },
            submission::ErrorCase { literal: "purchase_order_not_open", required: &["field"], sources: &["transaction_invariant"] },
            submission::ErrorCase { literal: "quantity_exceeds_remaining", required: &["field", "id"], sources: &["transaction_invariant"] },
            submission::ErrorCase { literal: "receipt_reference_conflict", required: &["constraint"], sources: &["transaction_invariant"] },
            submission::ErrorCase { literal: "retry", required: &[], sources: &["connection_unavailable", "serialization_failure"] },
            submission::ErrorCase { literal: "timeout", required: &[], sources: &["statement_timeout"] },
        ],
        kind: "command",
        transaction: Some("explicit_per_input"),
        direct: true,
        replay: submission::Replay::Unknown,
    },
    route: Some(crate::receiving::record_receipt_route),
    record: None,
    revision: None,
    revision_inputs: &[],
    requires_composition: false,
    supplied: &[
        screen::SuppliedField { path: "request_id", kind: screen::SuppliedKind::RequestId },
        screen::SuppliedField { path: "value.idempotency_key", kind: screen::SuppliedKind::IdempotencyKey },
        screen::SuppliedField { path: "value.occurred_at", kind: screen::SuppliedKind::OccurredAt },
    ],
};

#[must_use]
pub fn record_receipt(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&RECORD_RECEIPT_SPEC, binding)
}
