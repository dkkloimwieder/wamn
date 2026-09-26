// @generated; do not edit.

use wamn_client_tui::{screen, submission};

pub static GET_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "sample",
    name: "get",
    operation: "edge-samples:sample/get@1.0.0",
    kind: "get",
    input: crate::sample::SAMPLE_GET_INPUT_SCHEMA,
    input_schema: Some(
        "{\"items\":{\"additionalProperties\":false,\"properties\":{\"id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"}},\"required\":[\"id\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}",
    ),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::sample::SAMPLE_GET_RESULT_SCHEMA,
        result_class: Some("one"),
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
                literal: "not_found",
                required: &["field", "id"],
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
        kind: "get",
        transaction: Some("implicit"),
        direct: true,
        replay: submission::Replay::Unknown,
    },
    route: Some(crate::sample::get_route),
    fresh_only: false,
    record: Some(screen::RecordLink {
        relation: "edge_samples.sample",
        key_field: "id",
        key_input: Some("id"),
    }),
    revision: None,
    revision_inputs: &[],
    requires_composition: false,
    supplied: &[],
};

#[must_use]
pub fn get(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&GET_SPEC, binding)
}

pub static RECORD_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "sample",
    name: "record",
    operation: "edge-samples:sample/record@1.0.0",
    kind: "command",
    input: crate::sample::SAMPLE_RECORD_INPUT_SCHEMA,
    input_schema: Some(
        "{\"items\":{\"additionalProperties\":false,\"properties\":{\"request_id\":{\"minLength\":1,\"type\":\"string\"},\"value\":{\"additionalProperties\":false,\"properties\":{\"captured_at\":{\"format\":\"date-time\",\"type\":\"string\"},\"frame\":{\"minLength\":1,\"type\":\"string\"},\"idempotency_key\":{\"minLength\":1,\"type\":\"string\"}},\"required\":[\"idempotency_key\",\"frame\",\"captured_at\"],\"type\":\"object\"}},\"required\":[\"request_id\",\"value\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}",
    ),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::sample::SAMPLE_RECORD_RESULT_SCHEMA,
        result_class: Some("one"),
        errors: &[
            submission::ErrorCase {
                literal: "idempotency_conflict",
                required: &["field"],
                sources: &["same_key_different_canonical_command"],
            },
            submission::ErrorCase {
                literal: "internal_error",
                required: &[],
                sources: &["query_error", "row_limit_exceeded", "undeclared_constraint"],
            },
            submission::ErrorCase {
                literal: "invalid_input",
                required: &["field"],
                sources: &["envelope_count", "malformed_input"],
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
        kind: "command",
        transaction: Some("explicit_per_input"),
        direct: true,
        replay: submission::Replay::Claim,
    },
    route: Some(crate::sample::record_route),
    fresh_only: false,
    record: None,
    revision: None,
    revision_inputs: &[],
    requires_composition: false,
    supplied: &[
        screen::SuppliedField {
            path: "request_id",
            kind: screen::SuppliedKind::RequestId,
        },
        screen::SuppliedField {
            path: "value.idempotency_key",
            kind: screen::SuppliedKind::IdempotencyKey,
        },
    ],
};

#[must_use]
pub fn record(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&RECORD_SPEC, binding)
}
