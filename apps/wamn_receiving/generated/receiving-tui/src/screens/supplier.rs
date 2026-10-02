// @generated; do not edit.

use wamn_client_tui::{screen, submission};

pub static CREATE_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "supplier",
    name: "create",
    operation: "wamn-receiving:supplier/create@2.1.0",
    type_: "create",
    input: crate::supplier::SUPPLIER_CREATE_INPUT_SCHEMA,
    input_schema: Some(
        "{\"items\":{\"additionalProperties\":false,\"properties\":{\"idempotency_key\":{\"minLength\":1,\"type\":\"string\"},\"name\":{\"minLength\":1,\"type\":[\"string\",\"null\"],\"x-wamn-explicit-null\":\"invalid_input\"},\"request_id\":{\"minLength\":1,\"type\":\"string\"}},\"required\":[\"idempotency_key\",\"request_id\",\"name\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}",
    ),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::supplier::SUPPLIER_CREATE_RESULT_SCHEMA,
        result_class: Some("one"),
        errors: &[
            submission::ErrorCase {
                literal: "idempotency_conflict",
                required: &["field"],
                sources: &["changed_request"],
            },
            submission::ErrorCase {
                literal: "internal_error",
                required: &[],
                sources: &["query_error", "row_limit_exceeded"],
            },
            submission::ErrorCase {
                literal: "invalid_input",
                required: &["field"],
                sources: &[],
            },
            submission::ErrorCase {
                literal: "permission_denied",
                required: &["operation"],
                sources: &["permission_denied"],
            },
            submission::ErrorCase {
                literal: "retry",
                required: &[],
                sources: &["connection_unavailable", "serialization_failure"],
            },
            submission::ErrorCase {
                literal: "timeout",
                required: &[],
                sources: &["statement_timeout"],
            },
            submission::ErrorCase {
                literal: "unique_violation",
                required: &["constraint"],
                sources: &["unique_violation"],
            },
        ],
        type_: "create",
        transaction: Some("explicit_per_input"),
        direct: true,
        replay: submission::Replay::Claim,
    },
    route: Some(crate::supplier::create_route),
    fresh_only: false,
    record: Some(screen::RecordLink {
        relation: "receiving.supplier",
        key_field: "id",
        key_input: None,
    }),
    revision: None,
    revision_inputs: &[],
    requires_composition: false,
    supplied: &[
        screen::SuppliedField {
            path: "idempotency_key",
            type_: screen::SuppliedType::IdempotencyKey,
        },
        screen::SuppliedField {
            path: "request_id",
            type_: screen::SuppliedType::RequestId,
        },
    ],
};

#[must_use]
pub fn create(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&CREATE_SPEC, binding)
}

pub static QUERY_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "supplier",
    name: "query",
    operation: "wamn-receiving:supplier/query@2.1.0",
    type_: "query",
    input: crate::supplier::SUPPLIER_QUERY_INPUT_SCHEMA,
    input_schema: Some(
        "{\"items\":{\"additionalProperties\":false,\"properties\":{\"cursor\":{\"minLength\":1,\"type\":\"string\"},\"limit\":{\"maximum\":100,\"minimum\":1,\"type\":\"integer\"}},\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}",
    ),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::supplier::SUPPLIER_QUERY_RESULT_SCHEMA,
        result_class: Some("page"),
        errors: &[
            submission::ErrorCase {
                literal: "internal_error",
                required: &[],
                sources: &["query_error", "row_limit_exceeded"],
            },
            submission::ErrorCase {
                literal: "invalid_input",
                required: &["field"],
                sources: &[],
            },
            submission::ErrorCase {
                literal: "permission_denied",
                required: &["operation"],
                sources: &["permission_denied"],
            },
            submission::ErrorCase {
                literal: "retry",
                required: &[],
                sources: &["connection_unavailable", "serialization_failure"],
            },
            submission::ErrorCase {
                literal: "timeout",
                required: &[],
                sources: &["statement_timeout"],
            },
        ],
        type_: "query",
        transaction: Some("implicit"),
        direct: true,
        replay: submission::Replay::Unknown,
    },
    route: Some(crate::supplier::query_route),
    fresh_only: false,
    record: Some(screen::RecordLink {
        relation: "receiving.supplier",
        key_field: "id",
        key_input: None,
    }),
    revision: None,
    revision_inputs: &[],
    requires_composition: false,
    supplied: &[],
};

#[must_use]
pub fn query(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&QUERY_SPEC, binding)
}
