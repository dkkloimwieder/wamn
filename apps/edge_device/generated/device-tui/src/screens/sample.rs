// @generated; do not edit.

use wamn_client_tui::{screen, submission};

pub static READ_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "sample",
    name: "read",
    operation: "edge-device:sample/read@1.0.0",
    kind: "command",
    input: crate::sample::SAMPLE_READ_INPUT_SCHEMA,
    input_schema: Some(
        "{\"items\":{\"additionalProperties\":false,\"properties\":{\"request_id\":{\"minLength\":1,\"type\":\"string\"},\"value\":{\"additionalProperties\":false,\"properties\":{\"captured_at\":{\"format\":\"date-time\",\"type\":\"string\"},\"frame\":{\"minLength\":1,\"type\":\"string\"}},\"required\":[\"frame\",\"captured_at\"],\"type\":\"object\"}},\"required\":[\"request_id\",\"value\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}",
    ),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::sample::SAMPLE_READ_RESULT_SCHEMA,
        result_class: Some("one"),
        errors: &[
            submission::ErrorCase {
                literal: "internal_error",
                required: &[],
                sources: &["query_error", "row_limit_exceeded", "undeclared_constraint"],
            },
            submission::ErrorCase {
                literal: "invalid_input",
                required: &["field"],
                sources: &["malformed_input"],
            },
            submission::ErrorCase {
                literal: "permission_denied",
                required: &["operation"],
                sources: &["permission_denied"],
            },
        ],
        kind: "command",
        transaction: None,
        direct: true,
        replay: submission::Replay::Unknown,
    },
    route: Some(crate::sample::read_route),
    fresh_only: false,
    record: None,
    revision: None,
    revision_inputs: &[],
    requires_composition: false,
    supplied: &[screen::SuppliedField {
        path: "request_id",
        kind: screen::SuppliedKind::RequestId,
    }],
};

#[must_use]
pub fn read(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&READ_SPEC, binding)
}
