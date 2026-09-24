// @generated; do not edit.

use wamn_client_tui::{screen, submission};

pub static LIST_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "widget_maker",
    name: "list",
    operation: "platform-fixture:widget-maker/list@1.0.0",
    kind: "projection",
    input: crate::widget_maker::WIDGET_MAKER_LIST_INPUT_SCHEMA,
    input_schema: None,
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::widget_maker::WIDGET_MAKER_LIST_RESULT_SCHEMA,
        result_class: Some("bounded_list"),
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
        kind: "projection",
        transaction: None,
        direct: true,
        replay: submission::Replay::Unknown,
    },
    route: Some(crate::widget_maker::list_route),
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
pub fn list(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&LIST_SPEC, binding)
}

pub static QUERY_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "widget_maker",
    name: "query",
    operation: "platform-fixture:widget-maker/query@1.0.0",
    kind: "query",
    input: crate::widget_maker::WIDGET_MAKER_QUERY_INPUT_SCHEMA,
    input_schema: Some(
        "{\"items\":{\"additionalProperties\":false,\"properties\":{\"cursor\":{\"minLength\":1,\"type\":\"string\"},\"filter\":{\"additionalProperties\":false,\"properties\":{\"name\":{\"items\":{\"type\":\"string\"},\"type\":\"array\"}},\"type\":\"object\"},\"limit\":{\"maximum\":100,\"minimum\":1,\"type\":\"integer\"},\"request_id\":{\"minLength\":1,\"type\":\"string\"},\"sort\":{\"additionalProperties\":false,\"properties\":{\"direction\":{\"enum\":[\"ascending\",\"descending\"]},\"field\":{\"enum\":[\"created_at\"]}},\"required\":[\"field\",\"direction\"],\"type\":\"object\"}},\"required\":[\"request_id\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}",
    ),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::widget_maker::WIDGET_MAKER_QUERY_RESULT_SCHEMA,
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
        kind: "query",
        transaction: Some("implicit"),
        direct: true,
        replay: submission::Replay::Unknown,
    },
    route: Some(crate::widget_maker::query_route),
    fresh_only: false,
    record: Some(screen::RecordLink {
        relation: "inventory.widget_maker",
        key_field: "id",
        key_input: None,
    }),
    revision: None,
    revision_inputs: &[],
    requires_composition: false,
    supplied: &[screen::SuppliedField {
        path: "request_id",
        kind: screen::SuppliedKind::RequestId,
    }],
};

#[must_use]
pub fn query(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&QUERY_SPEC, binding)
}
