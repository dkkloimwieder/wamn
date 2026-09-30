// @generated; do not edit.

use wamn_client_tui::{screen, submission};

pub static ADJUST_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "inventory",
    name: "adjust",
    operation: "wamn-wms:inventory/adjust@1.0.0",
    type_: "command",
    input: crate::inventory::INVENTORY_ADJUST_INPUT_SCHEMA,
    input_schema: Some(
        "{\"items\":{\"additionalProperties\":false,\"properties\":{\"request_id\":{\"minLength\":1,\"type\":\"string\"},\"value\":{\"additionalProperties\":false,\"properties\":{\"expected_row_version\":{\"type\":\"integer\"},\"idempotency_key\":{\"minLength\":1,\"type\":\"string\"},\"occurred_at\":{\"format\":\"date-time\",\"type\":\"string\"},\"packaging_id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"},\"product_id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"},\"quantity\":{\"pattern\":\"^[0-9]+(\\\\.[0-9]+)?$\",\"type\":\"string\"},\"reason_code\":{\"minLength\":1,\"type\":\"string\"},\"status\":{\"enum\":[\"available\",\"held\"],\"type\":\"string\"}},\"required\":[\"idempotency_key\",\"packaging_id\",\"product_id\",\"status\",\"quantity\",\"reason_code\",\"expected_row_version\",\"occurred_at\"],\"type\":\"object\"}},\"required\":[\"request_id\",\"value\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}",
    ),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::inventory::INVENTORY_ADJUST_RESULT_SCHEMA,
        result_class: Some("one"),
        errors: &[
            submission::ErrorCase {
                literal: "concurrency_conflict",
                required: &["expected_row_version", "observed_row_version"],
                sources: &["transaction_invariant"],
            },
            submission::ErrorCase {
                literal: "idempotency_conflict",
                required: &["field"],
                sources: &["changed_request"],
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
                literal: "packaging_not_found",
                required: &["field", "id"],
                sources: &["transaction_invariant"],
            },
            submission::ErrorCase {
                literal: "permission_denied",
                required: &["operation"],
                sources: &["permission_denied"],
            },
            submission::ErrorCase {
                literal: "quantity_not_found",
                required: &["field", "id"],
                sources: &["transaction_invariant"],
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
        type_: "command",
        transaction: Some("explicit_per_input"),
        direct: true,
        replay: submission::Replay::Claim,
    },
    route: Some(crate::inventory::adjust_route),
    fresh_only: false,
    record: None,
    revision: None,
    revision_inputs: &["value.expected_row_version"],
    requires_composition: true,
    supplied: &[
        screen::SuppliedField {
            path: "request_id",
            type_: screen::SuppliedType::RequestId,
        },
        screen::SuppliedField {
            path: "value.idempotency_key",
            type_: screen::SuppliedType::IdempotencyKey,
        },
        screen::SuppliedField {
            path: "value.occurred_at",
            type_: screen::SuppliedType::OccurredAt,
        },
    ],
};

#[must_use]
pub fn adjust(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&ADJUST_SPEC, binding)
}

pub static AGGREGATE_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "inventory",
    name: "aggregate",
    operation: "wamn-wms:inventory/aggregate@1.0.0",
    type_: "projection",
    input: crate::inventory::INVENTORY_AGGREGATE_INPUT_SCHEMA,
    input_schema: Some(
        "{\"items\":{\"additionalProperties\":false,\"properties\":{},\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}",
    ),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::inventory::INVENTORY_AGGREGATE_RESULT_SCHEMA,
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
        type_: "projection",
        transaction: None,
        direct: true,
        replay: submission::Replay::Unknown,
    },
    route: Some(crate::inventory::aggregate_route),
    fresh_only: false,
    record: None,
    revision: None,
    revision_inputs: &[],
    requires_composition: false,
    supplied: &[],
};

#[must_use]
pub fn aggregate(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&AGGREGATE_SPEC, binding)
}

pub static MERGE_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "inventory",
    name: "merge",
    operation: "wamn-wms:inventory/merge@1.0.0",
    type_: "command",
    input: crate::inventory::INVENTORY_MERGE_INPUT_SCHEMA,
    input_schema: Some(
        "{\"items\":{\"additionalProperties\":false,\"properties\":{\"request_id\":{\"minLength\":1,\"type\":\"string\"},\"value\":{\"additionalProperties\":false,\"properties\":{\"expected_row_version\":{\"type\":\"integer\"},\"idempotency_key\":{\"minLength\":1,\"type\":\"string\"},\"occurred_at\":{\"format\":\"date-time\",\"type\":\"string\"},\"source_packaging_id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"},\"target_packaging_id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"}},\"required\":[\"idempotency_key\",\"source_packaging_id\",\"target_packaging_id\",\"expected_row_version\",\"occurred_at\"],\"type\":\"object\"}},\"required\":[\"request_id\",\"value\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}",
    ),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::inventory::INVENTORY_MERGE_RESULT_SCHEMA,
        result_class: Some("one"),
        errors: &[
            submission::ErrorCase {
                literal: "concurrency_conflict",
                required: &["expected_row_version", "observed_row_version"],
                sources: &["transaction_invariant"],
            },
            submission::ErrorCase {
                literal: "idempotency_conflict",
                required: &["field"],
                sources: &["changed_request"],
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
                literal: "packaging_not_found",
                required: &["field", "id"],
                sources: &["transaction_invariant"],
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
        type_: "command",
        transaction: Some("explicit_per_input"),
        direct: true,
        replay: submission::Replay::Claim,
    },
    route: Some(crate::inventory::merge_route),
    fresh_only: false,
    record: None,
    revision: None,
    revision_inputs: &["value.expected_row_version"],
    requires_composition: true,
    supplied: &[
        screen::SuppliedField {
            path: "request_id",
            type_: screen::SuppliedType::RequestId,
        },
        screen::SuppliedField {
            path: "value.idempotency_key",
            type_: screen::SuppliedType::IdempotencyKey,
        },
        screen::SuppliedField {
            path: "value.occurred_at",
            type_: screen::SuppliedType::OccurredAt,
        },
    ],
};

#[must_use]
pub fn merge(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&MERGE_SPEC, binding)
}

pub static MOVE_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "inventory",
    name: "move",
    operation: "wamn-wms:inventory/move@1.0.0",
    type_: "command",
    input: crate::inventory::INVENTORY_MOVE_INPUT_SCHEMA,
    input_schema: Some(
        "{\"items\":{\"additionalProperties\":false,\"properties\":{\"request_id\":{\"minLength\":1,\"type\":\"string\"},\"value\":{\"additionalProperties\":false,\"properties\":{\"expected_row_version\":{\"type\":\"integer\"},\"idempotency_key\":{\"minLength\":1,\"type\":\"string\"},\"occurred_at\":{\"format\":\"date-time\",\"type\":\"string\"},\"packaging_id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"},\"to_location_id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"}},\"required\":[\"idempotency_key\",\"packaging_id\",\"to_location_id\",\"expected_row_version\",\"occurred_at\"],\"type\":\"object\"}},\"required\":[\"request_id\",\"value\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}",
    ),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::inventory::INVENTORY_MOVE_RESULT_SCHEMA,
        result_class: Some("one"),
        errors: &[
            submission::ErrorCase {
                literal: "concurrency_conflict",
                required: &["expected_row_version", "observed_row_version"],
                sources: &["transaction_invariant"],
            },
            submission::ErrorCase {
                literal: "idempotency_conflict",
                required: &["field"],
                sources: &["changed_request"],
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
                literal: "location_not_found",
                required: &["field", "id"],
                sources: &["transaction_invariant"],
            },
            submission::ErrorCase {
                literal: "packaging_not_found",
                required: &["field", "id"],
                sources: &["transaction_invariant"],
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
        type_: "command",
        transaction: Some("explicit_per_input"),
        direct: true,
        replay: submission::Replay::Claim,
    },
    route: Some(crate::inventory::move_route),
    fresh_only: false,
    record: None,
    revision: None,
    revision_inputs: &["value.expected_row_version"],
    requires_composition: true,
    supplied: &[
        screen::SuppliedField {
            path: "request_id",
            type_: screen::SuppliedType::RequestId,
        },
        screen::SuppliedField {
            path: "value.idempotency_key",
            type_: screen::SuppliedType::IdempotencyKey,
        },
        screen::SuppliedField {
            path: "value.occurred_at",
            type_: screen::SuppliedType::OccurredAt,
        },
    ],
};

#[must_use]
pub fn move_(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&MOVE_SPEC, binding)
}

pub static SPLIT_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "inventory",
    name: "split",
    operation: "wamn-wms:inventory/split@1.0.0",
    type_: "command",
    input: crate::inventory::INVENTORY_SPLIT_INPUT_SCHEMA,
    input_schema: Some(
        "{\"items\":{\"additionalProperties\":false,\"properties\":{\"request_id\":{\"minLength\":1,\"type\":\"string\"},\"value\":{\"additionalProperties\":false,\"properties\":{\"expected_row_version\":{\"type\":\"integer\"},\"idempotency_key\":{\"minLength\":1,\"type\":\"string\"},\"new_packaging_code\":{\"minLength\":1,\"type\":\"string\"},\"new_packaging_type\":{\"enum\":[\"pallet\",\"tote\",\"bin\",\"case\",\"loose\"],\"type\":\"string\"},\"occurred_at\":{\"format\":\"date-time\",\"type\":\"string\"},\"product_id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"},\"quantity\":{\"pattern\":\"^[0-9]+(\\\\.[0-9]+)?$\",\"type\":\"string\"},\"source_packaging_id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"},\"status\":{\"enum\":[\"available\",\"held\"],\"type\":\"string\"},\"to_location_id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"}},\"required\":[\"idempotency_key\",\"source_packaging_id\",\"product_id\",\"status\",\"quantity\",\"new_packaging_code\",\"new_packaging_type\",\"to_location_id\",\"expected_row_version\",\"occurred_at\"],\"type\":\"object\"}},\"required\":[\"request_id\",\"value\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}",
    ),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::inventory::INVENTORY_SPLIT_RESULT_SCHEMA,
        result_class: Some("one"),
        errors: &[
            submission::ErrorCase {
                literal: "concurrency_conflict",
                required: &["expected_row_version", "observed_row_version"],
                sources: &["transaction_invariant"],
            },
            submission::ErrorCase {
                literal: "idempotency_conflict",
                required: &["field"],
                sources: &["changed_request"],
            },
            submission::ErrorCase {
                literal: "insufficient_quantity",
                required: &["field"],
                sources: &["transaction_invariant"],
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
                literal: "location_not_found",
                required: &["field", "id"],
                sources: &["transaction_invariant"],
            },
            submission::ErrorCase {
                literal: "packaging_not_found",
                required: &["field", "id"],
                sources: &["transaction_invariant"],
            },
            submission::ErrorCase {
                literal: "permission_denied",
                required: &["operation"],
                sources: &["permission_denied"],
            },
            submission::ErrorCase {
                literal: "quantity_not_found",
                required: &["field", "id"],
                sources: &["transaction_invariant"],
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
        type_: "command",
        transaction: Some("explicit_per_input"),
        direct: true,
        replay: submission::Replay::Claim,
    },
    route: Some(crate::inventory::split_route),
    fresh_only: false,
    record: None,
    revision: None,
    revision_inputs: &["value.expected_row_version"],
    requires_composition: true,
    supplied: &[
        screen::SuppliedField {
            path: "request_id",
            type_: screen::SuppliedType::RequestId,
        },
        screen::SuppliedField {
            path: "value.idempotency_key",
            type_: screen::SuppliedType::IdempotencyKey,
        },
        screen::SuppliedField {
            path: "value.occurred_at",
            type_: screen::SuppliedType::OccurredAt,
        },
    ],
};

#[must_use]
pub fn split(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&SPLIT_SPEC, binding)
}
