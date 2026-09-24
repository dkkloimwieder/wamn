// @generated; do not edit.

use wamn_client_tui::{screen, submission};

pub static CREATE_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "product",
    name: "create",
    operation: "wamn-wms:product/create@1.0.0",
    kind: "create",
    input: crate::product::PRODUCT_CREATE_INPUT_SCHEMA,
    input_schema: Some(
        "{\"items\":{\"additionalProperties\":false,\"properties\":{\"idempotency_key\":{\"minLength\":1,\"type\":\"string\"},\"product_code\":{\"minLength\":1,\"type\":[\"string\",\"null\"],\"x-wamn-explicit-null\":\"invalid_input\"},\"request_id\":{\"minLength\":1,\"type\":\"string\"}},\"required\":[\"idempotency_key\",\"request_id\",\"product_code\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}",
    ),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::product::PRODUCT_CREATE_RESULT_SCHEMA,
        result_class: Some("one"),
        errors: &[
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
    route: Some(crate::product::create_route),
    fresh_only: false,
    record: Some(screen::RecordLink {
        relation: "wms.product",
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
    model: "product",
    name: "get",
    operation: "wamn-wms:product/get@1.0.0",
    kind: "get",
    input: crate::product::PRODUCT_GET_INPUT_SCHEMA,
    input_schema: Some(
        "{\"items\":{\"additionalProperties\":false,\"properties\":{\"id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"},\"request_id\":{\"minLength\":1,\"type\":\"string\"}},\"required\":[\"id\",\"request_id\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}",
    ),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::product::PRODUCT_GET_RESULT_SCHEMA,
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
    route: Some(crate::product::get_route),
    fresh_only: false,
    record: Some(screen::RecordLink {
        relation: "wms.product",
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
    model: "product",
    name: "query",
    operation: "wamn-wms:product/query@1.0.0",
    kind: "query",
    input: crate::product::PRODUCT_QUERY_INPUT_SCHEMA,
    input_schema: Some(
        "{\"items\":{\"additionalProperties\":false,\"properties\":{\"cursor\":{\"minLength\":1,\"type\":\"string\"},\"filter\":{\"additionalProperties\":false,\"properties\":{\"product_code\":{\"items\":{\"type\":\"string\"},\"type\":\"array\"}},\"type\":\"object\"},\"limit\":{\"maximum\":100,\"minimum\":1,\"type\":\"integer\"},\"request_id\":{\"minLength\":1,\"type\":\"string\"}},\"required\":[\"request_id\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}",
    ),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::product::PRODUCT_QUERY_RESULT_SCHEMA,
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
    route: Some(crate::product::query_route),
    fresh_only: false,
    record: Some(screen::RecordLink {
        relation: "wms.product",
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

pub static UPDATE_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "product",
    name: "update",
    operation: "wamn-wms:product/update@1.0.0",
    kind: "update",
    input: crate::product::PRODUCT_UPDATE_INPUT_SCHEMA,
    input_schema: Some(
        "{\"items\":{\"additionalProperties\":false,\"properties\":{\"change\":{\"additionalProperties\":false,\"properties\":{\"product_code\":{\"type\":[\"string\",\"null\"],\"x-wamn-explicit-null\":\"invalid_input\"}},\"type\":\"object\"},\"expected_row_version\":{\"type\":\"integer\"},\"id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"},\"request_id\":{\"minLength\":1,\"type\":\"string\"}},\"required\":[\"expected_row_version\",\"id\",\"request_id\",\"change\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}",
    ),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::product::PRODUCT_UPDATE_RESULT_SCHEMA,
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
            submission::ErrorCase {
                literal: "unique_violation",
                required: &["constraint"],
                sources: &["unique_violation"],
            },
        ],
        kind: "update",
        transaction: Some("implicit"),
        direct: true,
        replay: submission::Replay::Unknown,
    },
    route: Some(crate::product::update_route),
    fresh_only: false,
    record: Some(screen::RecordLink {
        relation: "wms.product",
        key_field: "id",
        key_input: Some("id"),
    }),
    revision: Some(screen::RevisionBinding {
        read_operation: "wamn-wms:product/get@1.0.0",
        read_key_input: "id",
        key_field: "id",
        revision_field: "row_version",
        command_key_input: "id",
        command_revision_input: "expected_row_version",
    }),
    revision_inputs: &["expected_row_version"],
    requires_composition: false,
    supplied: &[screen::SuppliedField {
        path: "request_id",
        kind: screen::SuppliedKind::RequestId,
    }],
};

#[must_use]
pub fn update(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&UPDATE_SPEC, binding)
}
