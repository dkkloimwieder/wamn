// @generated; do not edit.

use wamn_client_tui::{screen, submission};

pub static CREATE_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "pallet",
    name: "create",
    operation: "wamn-wms:pallet/create@1.0.0",
    kind: "create",
    input: crate::pallet::PALLET_CREATE_INPUT_SCHEMA,
    input_schema: Some(
        "{\"items\":{\"additionalProperties\":false,\"properties\":{\"idempotency_key\":{\"minLength\":1,\"type\":\"string\"},\"location_id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":[\"string\",\"null\"],\"x-wamn-explicit-null\":\"invalid_input\"},\"pallet_code\":{\"minLength\":1,\"type\":[\"string\",\"null\"],\"x-wamn-explicit-null\":\"invalid_input\"},\"request_id\":{\"minLength\":1,\"type\":\"string\"},\"status\":{\"enum\":[\"available\",\"held\",null],\"type\":[\"string\",\"null\"],\"x-wamn-explicit-null\":\"invalid_input\"}},\"required\":[\"idempotency_key\",\"request_id\",\"pallet_code\",\"location_id\",\"status\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}",
    ),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::pallet::PALLET_CREATE_RESULT_SCHEMA,
        result_class: Some("one"),
        errors: &[
            submission::ErrorCase {
                literal: "check_violation",
                required: &["constraint"],
                sources: &["check_violation"],
            },
            submission::ErrorCase {
                literal: "foreign_key_violation",
                required: &["constraint"],
                sources: &["foreign_key_violation"],
            },
            submission::ErrorCase {
                literal: "idempotency_conflict",
                required: &["field"],
                sources: &["changed_canonical_command"],
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
        kind: "create",
        transaction: Some("explicit_per_input"),
        direct: true,
        replay: submission::Replay::Claim,
    },
    route: Some(crate::pallet::create_route),
    fresh_only: false,
    record: Some(screen::RecordLink {
        relation: "wms.pallet",
        key_field: "id",
        key_input: None,
    }),
    revision: None,
    revision_inputs: &[],
    requires_composition: false,
    supplied: &[
        screen::SuppliedField {
            path: "idempotency_key",
            kind: screen::SuppliedKind::IdempotencyKey,
        },
        screen::SuppliedField {
            path: "request_id",
            kind: screen::SuppliedKind::RequestId,
        },
    ],
};

#[must_use]
pub fn create(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&CREATE_SPEC, binding)
}

pub static GET_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "pallet",
    name: "get",
    operation: "wamn-wms:pallet/get@1.0.0",
    kind: "get",
    input: crate::pallet::PALLET_GET_INPUT_SCHEMA,
    input_schema: Some(
        "{\"items\":{\"additionalProperties\":false,\"properties\":{\"id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"},\"request_id\":{\"minLength\":1,\"type\":\"string\"}},\"required\":[\"id\",\"request_id\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}",
    ),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::pallet::PALLET_GET_RESULT_SCHEMA,
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
    route: Some(crate::pallet::get_route),
    fresh_only: false,
    record: Some(screen::RecordLink {
        relation: "wms.pallet",
        key_field: "id",
        key_input: Some("id"),
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
pub fn get(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&GET_SPEC, binding)
}

pub static QUERY_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "pallet",
    name: "query",
    operation: "wamn-wms:pallet/query@1.0.0",
    kind: "query",
    input: crate::pallet::PALLET_QUERY_INPUT_SCHEMA,
    input_schema: Some(
        "{\"items\":{\"additionalProperties\":false,\"properties\":{\"cursor\":{\"minLength\":1,\"type\":\"string\"},\"filter\":{\"additionalProperties\":false,\"properties\":{\"location_id\":{\"items\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"},\"type\":\"array\"},\"pallet_code\":{\"items\":{\"type\":\"string\"},\"type\":\"array\"},\"status\":{\"items\":{\"enum\":[\"available\",\"held\",\"consumed\"],\"type\":\"string\"},\"type\":\"array\"}},\"type\":\"object\"},\"limit\":{\"maximum\":100,\"minimum\":1,\"type\":\"integer\"},\"request_id\":{\"minLength\":1,\"type\":\"string\"},\"sort\":{\"additionalProperties\":false,\"properties\":{\"direction\":{\"enum\":[\"ascending\",\"descending\"]},\"field\":{\"enum\":[\"pallet_code\",\"location_id\",\"updated_at\",\"created_at\"]}},\"required\":[\"field\",\"direction\"],\"type\":\"object\"}},\"required\":[\"request_id\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}",
    ),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::pallet::PALLET_QUERY_RESULT_SCHEMA,
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
    route: Some(crate::pallet::query_route),
    fresh_only: false,
    record: Some(screen::RecordLink {
        relation: "wms.pallet",
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
