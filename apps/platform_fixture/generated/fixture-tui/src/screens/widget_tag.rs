// @generated; do not edit.

use wamn_client_tui::{screen, submission};

pub static UPDATE_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "widget_tag",
    name: "update",
    operation: "platform-fixture:widget-tag/update@1.0.0",
    kind: "update",
    input: crate::widget_tag::WIDGET_TAG_UPDATE_INPUT_SCHEMA,
    input_schema: Some(
        "{\"items\":{\"properties\":{\"change\":{\"properties\":{\"label\":{\"type\":[\"string\",\"null\"],\"x-wamn-explicit-null\":\"invalid_input\"}},\"type\":\"object\"},\"expected_edit_version\":{\"type\":\"string\"},\"id\":{\"format\":\"uuid\",\"type\":\"string\"},\"request_id\":{\"type\":\"string\"}},\"required\":[\"id\",\"expected_edit_version\",\"request_id\"],\"type\":\"object\"},\"type\":\"array\"}",
    ),
    response: submission::ResponseContract {
        schema: None,
        partial_schema: None,
        fields: crate::widget_tag::WIDGET_TAG_UPDATE_RESULT_SCHEMA,
        result_class: Some("one"),
        errors: &[
            submission::ErrorCase {
                literal: "concurrency_conflict",
                required: &["expected_row_version", "observed_row_version"],
                sources: &[],
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
        kind: "update",
        transaction: Some("implicit"),
        direct: true,
        replay: submission::Replay::Unknown,
    },
    route: Some(crate::widget_tag::update_route),
    fresh_only: false,
    record: Some(screen::RecordLink {
        relation: "inventory.widget_tag",
        key_field: "id",
        key_input: Some("id"),
    }),
    revision: None,
    revision_inputs: &["expected_edit_version"],
    requires_composition: true,
    supplied: &[screen::SuppliedField {
        path: "request_id",
        kind: screen::SuppliedKind::RequestId,
    }],
};

#[must_use]
pub fn update(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&UPDATE_SPEC, binding)
}
