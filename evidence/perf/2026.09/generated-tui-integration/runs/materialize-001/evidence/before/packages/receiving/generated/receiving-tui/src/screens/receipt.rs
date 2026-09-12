// @generated; do not edit.

use wamn_client_tui::{screen, submission};

pub static GET_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "receipt",
    name: "get",
    operation: "wamn-receiving:receipt/get@1.0.0",
    kind: "get",
    input: crate::receipt::RECEIPT_GET_INPUT_SCHEMA,
    input_schema: Some("{\"items\":{\"additionalProperties\":false,\"properties\":{\"id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"},\"request_id\":{\"minLength\":1,\"type\":\"string\"}},\"required\":[\"request_id\",\"id\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}"),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        fields: crate::receipt::RECEIPT_GET_RESULT_SCHEMA,
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
    route: Some(crate::receipt::get_route),
    record: Some(screen::RecordLink { relation: "receiving.receipt", key_field: "id", key_input: Some("id") }),
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

pub static QUERY_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "receipt",
    name: "query",
    operation: "wamn-receiving:receipt/query@1.0.0",
    kind: "query",
    input: crate::receipt::RECEIPT_QUERY_INPUT_SCHEMA,
    input_schema: Some("{\"items\":{\"additionalProperties\":false,\"properties\":{\"cursor\":{\"minLength\":1,\"type\":\"string\"},\"limit\":{\"maximum\":100,\"minimum\":1,\"type\":\"integer\"},\"request_id\":{\"minLength\":1,\"type\":\"string\"}},\"required\":[\"request_id\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}"),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        fields: crate::receipt::RECEIPT_QUERY_RESULT_SCHEMA,
        result_class: Some("page"),
        errors: &[
            submission::ErrorCase { literal: "internal_error", required: &[], sources: &["query_error", "row_limit_exceeded"] },
            submission::ErrorCase { literal: "invalid_input", required: &["field"], sources: &[] },
            submission::ErrorCase { literal: "permission_denied", required: &["operation"], sources: &["permission_denied"] },
            submission::ErrorCase { literal: "retry", required: &[], sources: &["connection_unavailable", "serialization_failure"] },
            submission::ErrorCase { literal: "timeout", required: &[], sources: &["statement_timeout"] },
        ],
        kind: "query",
        transaction: Some("implicit"),
        direct: true,
        replay: submission::Replay::Unknown,
    },
    route: Some(crate::receipt::query_route),
    record: Some(screen::RecordLink { relation: "receiving.receipt", key_field: "id", key_input: None }),
    revision: None,
    revision_inputs: &[],
    requires_composition: false,
    supplied: &[
        screen::SuppliedField { path: "request_id", kind: screen::SuppliedKind::RequestId },
    ],
};

#[must_use]
pub fn query(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&QUERY_SPEC, binding)
}
