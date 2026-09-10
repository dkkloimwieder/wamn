// @generated; do not edit.

use wamn_client_tui::{screen, submission};

pub static APPROVE_INSPECTION_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "quality",
    name: "approve_inspection",
    operation: "client-acme-receiving:quality/approve-inspection@3.0.0",
    kind: "command",
    input: crate::quality::QUALITY_APPROVE_INSPECTION_INPUT_SCHEMA,
    input_schema: Some("{\"items\":{\"additionalProperties\":false,\"properties\":{\"expected_row_version\":{\"pattern\":\"^-?(0|[1-9][0-9]*)$\",\"type\":\"string\"},\"receipt_id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"},\"request_id\":{\"minLength\":1,\"type\":\"string\"}},\"required\":[\"request_id\",\"receipt_id\",\"expected_row_version\"],\"type\":\"object\"},\"type\":\"array\"}"),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::quality::QUALITY_APPROVE_INSPECTION_RESULT_SCHEMA,
        result_class: Some("one"),
        errors: &[
            submission::ErrorCase { literal: "concurrency_conflict", required: &["expected_row_version", "observed_row_version"], sources: &["transaction_invariant"] },
            submission::ErrorCase { literal: "internal_error", required: &[], sources: &["query_error", "row_limit_exceeded", "undeclared_constraint"] },
            submission::ErrorCase { literal: "invalid_input", required: &["field"], sources: &["malformed_input"] },
            submission::ErrorCase { literal: "not_found", required: &["field", "id"], sources: &["transaction_invariant"] },
            submission::ErrorCase { literal: "permission_denied", required: &["operation"], sources: &["permission_denied"] },
            submission::ErrorCase { literal: "retry", required: &[], sources: &["connection_unavailable", "serialization_failure"] },
            submission::ErrorCase { literal: "timeout", required: &[], sources: &["statement_timeout"] },
        ],
        kind: "command",
        transaction: Some("explicit_per_input"),
        direct: true,
        replay: submission::Replay::State,
    },
    route: Some(crate::quality::approve_inspection_route),
    record: None,
    revision: None,
    revision_inputs: &["expected_row_version"],
    requires_composition: true,
    supplied: &[
        screen::SuppliedField { path: "request_id", kind: screen::SuppliedKind::RequestId },
    ],
};

#[must_use]
pub fn approve_inspection(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&APPROVE_INSPECTION_SPEC, binding)
}

pub static LOAD_PURCHASE_ORDER_DETAIL_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "quality",
    name: "load_purchase_order_detail",
    operation: "client-acme-receiving:quality/load-purchase-order-detail@3.0.0",
    kind: "projection",
    input: crate::quality::QUALITY_LOAD_PURCHASE_ORDER_DETAIL_INPUT_SCHEMA,
    input_schema: Some("{\"items\":{\"additionalProperties\":false,\"properties\":{\"purchase_order_id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"},\"request_id\":{\"minLength\":1,\"type\":\"string\"}},\"required\":[\"request_id\",\"purchase_order_id\"],\"type\":\"object\"},\"type\":\"array\"}"),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::quality::QUALITY_LOAD_PURCHASE_ORDER_DETAIL_RESULT_SCHEMA,
        result_class: Some("one"),
        errors: &[
            submission::ErrorCase { literal: "internal_error", required: &[], sources: &["query_error", "row_limit_exceeded", "undeclared_constraint"] },
            submission::ErrorCase { literal: "invalid_input", required: &["field"], sources: &["malformed_input"] },
            submission::ErrorCase { literal: "not_found", required: &["field", "id"], sources: &["transaction_invariant"] },
            submission::ErrorCase { literal: "permission_denied", required: &["operation"], sources: &["permission_denied"] },
            submission::ErrorCase { literal: "retry", required: &[], sources: &["connection_unavailable", "serialization_failure"] },
            submission::ErrorCase { literal: "timeout", required: &[], sources: &["statement_timeout"] },
        ],
        kind: "projection",
        transaction: None,
        direct: true,
        replay: submission::Replay::Unknown,
    },
    route: Some(crate::quality::load_purchase_order_detail_route),
    record: None,
    revision: None,
    revision_inputs: &[],
    requires_composition: false,
    supplied: &[
        screen::SuppliedField { path: "request_id", kind: screen::SuppliedKind::RequestId },
    ],
};

#[must_use]
pub fn load_purchase_order_detail(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&LOAD_PURCHASE_ORDER_DETAIL_SPEC, binding)
}
