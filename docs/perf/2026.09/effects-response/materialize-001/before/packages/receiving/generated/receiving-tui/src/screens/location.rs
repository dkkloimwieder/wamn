// @generated; do not edit.

use wamn_client_tui::{screen, submission};

pub static LIST_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "location",
    name: "list",
    operation: "wamn-receiving:location/list@1.0.0",
    kind: "projection",
    input: crate::location::LOCATION_LIST_INPUT_SCHEMA,
    input_schema: Some("{\"items\":{\"additionalProperties\":false,\"properties\":{\"request_id\":{\"minLength\":1,\"type\":\"string\"}},\"required\":[\"request_id\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}"),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        fields: crate::location::LOCATION_LIST_RESULT_SCHEMA,
        result_class: Some("bounded_list"),
        errors: &[
            submission::ErrorCase { literal: "internal_error", required: &[], sources: &["query_error", "row_limit_exceeded", "undeclared_constraint"] },
            submission::ErrorCase { literal: "invalid_input", required: &["field"], sources: &["malformed_input"] },
            submission::ErrorCase { literal: "permission_denied", required: &["operation"], sources: &["permission_denied"] },
            submission::ErrorCase { literal: "retry", required: &[], sources: &["connection_unavailable", "serialization_failure"] },
            submission::ErrorCase { literal: "timeout", required: &[], sources: &["statement_timeout"] },
        ],
        kind: "projection",
        transaction: None,
        direct: true,
        replay: submission::Replay::Unknown,
    },
    route: Some(crate::location::list_route),
    record: None,
    revision: None,
    revision_inputs: &[],
    requires_composition: false,
    supplied: &[
        screen::SuppliedField { path: "request_id", kind: screen::SuppliedKind::RequestId },
    ],
};

#[must_use]
pub fn list(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&LIST_SPEC, binding)
}
