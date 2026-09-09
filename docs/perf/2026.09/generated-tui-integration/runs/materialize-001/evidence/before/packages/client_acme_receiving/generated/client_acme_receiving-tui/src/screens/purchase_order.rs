// @generated; do not edit.

use wamn_client_tui::{screen, submission};

pub static GET_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "purchase_order",
    name: "get",
    operation: "client-acme-receiving:purchase-order/get@3.0.0",
    kind: "get",
    input: crate::purchase_order::PURCHASE_ORDER_GET_INPUT_SCHEMA,
    input_schema: Some("{\"items\":{\"additionalProperties\":false,\"properties\":{\"id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"},\"request_id\":{\"minLength\":1,\"type\":\"string\"}},\"required\":[\"request_id\",\"id\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}"),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        fields: crate::purchase_order::PURCHASE_ORDER_GET_RESULT_SCHEMA,
        result_class: Some("one"),
        errors: &[
            submission::ErrorCase { literal: "internal_error", required: &[], sources: &["query_error", "row_limit_exceeded"] },
            submission::ErrorCase { literal: "invalid_input", required: &["field"], sources: &[] },
            submission::ErrorCase { literal: "not_found", required: &["field", "id"], sources: &[] },
            submission::ErrorCase { literal: "permission_denied", required: &["operation"], sources: &["permission_denied"] },
            submission::ErrorCase { literal: "retry", required: &[], sources: &["connection_unavailable", "serialization_failure"] },
            submission::ErrorCase { literal: "timeout", required: &[], sources: &["statement_timeout"] },
        ],
        kind: "get",
        transaction: Some("implicit"),
        direct: true,
        replay: submission::Replay::Unknown,
    },
    route: Some(crate::purchase_order::get_route),
    record: Some(screen::RecordLink { relation: "receiving.purchase_order", key_field: "id", key_input: Some("id") }),
    revision: None,
    revision_inputs: &[],
    requires_composition: false,
    supplied: &[
        screen::SuppliedField { path: "request_id", kind: screen::SuppliedKind::RequestId },
    ],
};

#[must_use]
pub fn get(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&GET_SPEC, binding)
}

pub static UPDATE_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "purchase_order",
    name: "update",
    operation: "client-acme-receiving:purchase-order/update@3.0.0",
    kind: "update",
    input: crate::purchase_order::PURCHASE_ORDER_UPDATE_INPUT_SCHEMA,
    input_schema: Some("{\"items\":{\"additionalProperties\":false,\"properties\":{\"change\":{\"additionalProperties\":false,\"properties\":{\"acme_inspection_required\":{\"type\":[\"boolean\",\"null\"],\"x-wamn-explicit-null\":\"invalid_input\"},\"acme_quality_status\":{\"enum\":[\"not_required\",\"pending\",\"approved\",null],\"type\":[\"string\",\"null\"],\"x-wamn-explicit-null\":\"invalid_input\"}},\"type\":\"object\"},\"expected_row_version\":{\"pattern\":\"^-?(0|[1-9][0-9]*)$\",\"type\":\"string\"},\"id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"},\"request_id\":{\"minLength\":1,\"type\":\"string\"}},\"required\":[\"request_id\",\"id\",\"expected_row_version\",\"change\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}"),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        fields: crate::purchase_order::PURCHASE_ORDER_UPDATE_RESULT_SCHEMA,
        result_class: Some("one"),
        errors: &[
            submission::ErrorCase { literal: "concurrency_conflict", required: &["expected_row_version", "observed_row_version"], sources: &[] },
            submission::ErrorCase { literal: "internal_error", required: &[], sources: &["query_error", "row_limit_exceeded"] },
            submission::ErrorCase { literal: "invalid_input", required: &["field"], sources: &[] },
            submission::ErrorCase { literal: "not_found", required: &["field", "id"], sources: &[] },
            submission::ErrorCase { literal: "permission_denied", required: &["operation"], sources: &["permission_denied"] },
            submission::ErrorCase { literal: "retry", required: &[], sources: &["connection_unavailable", "serialization_failure"] },
            submission::ErrorCase { literal: "timeout", required: &[], sources: &["statement_timeout"] },
        ],
        kind: "update",
        transaction: Some("implicit"),
        direct: true,
        replay: submission::Replay::Unknown,
    },
    route: Some(crate::purchase_order::update_route),
    record: Some(screen::RecordLink { relation: "receiving.purchase_order", key_field: "id", key_input: Some("id") }),
    revision: Some(screen::RevisionBinding {
        read_operation: "client-acme-receiving:purchase-order/get@3.0.0",
        read_key_input: "id",
        key_field: "id",
        revision_field: "row_version",
        command_key_input: "id",
        command_revision_input: "expected_row_version",
    }),
    revision_inputs: &["expected_row_version"],
    requires_composition: false,
    supplied: &[
        screen::SuppliedField { path: "request_id", kind: screen::SuppliedKind::RequestId },
    ],
};

#[must_use]
pub fn update(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&UPDATE_SPEC, binding)
}
