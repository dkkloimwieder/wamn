// @generated; do not edit.

use wamn_client_tui::{screen, submission};

pub static ADJUST_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "inventory",
    name: "adjust",
    operation: "wamn-wms:inventory/adjust@1.0.0",
    kind: "command",
    input: crate::inventory::INVENTORY_ADJUST_INPUT_SCHEMA,
    input_schema: Some("{\"items\":{\"additionalProperties\":false,\"properties\":{\"request_id\":{\"minLength\":1,\"type\":\"string\"},\"value\":{\"additionalProperties\":false,\"properties\":{\"expected_row_version\":{\"minimum\":1,\"type\":\"integer\"},\"idempotency_key\":{\"minLength\":1,\"type\":\"string\"},\"occurred_at\":{\"format\":\"date-time\",\"type\":\"string\"},\"pallet_id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"},\"product_id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"},\"quantity\":{\"pattern\":\"^[0-9]+(\\\\.[0-9]+)?$\",\"type\":\"string\"},\"reason_code\":{\"minLength\":1,\"type\":\"string\"},\"status\":{\"enum\":[\"available\",\"held\"],\"type\":\"string\"}},\"required\":[\"idempotency_key\",\"pallet_id\",\"product_id\",\"status\",\"quantity\",\"reason_code\",\"expected_row_version\",\"occurred_at\"],\"type\":\"object\"}},\"required\":[\"request_id\",\"value\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}"),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        fields: crate::inventory::INVENTORY_ADJUST_RESULT_SCHEMA,
        result_class: Some("one"),
        errors: &[
            submission::ErrorCase { literal: "concurrency_conflict", required: &["expected_row_version", "observed_row_version"], sources: &["transaction_invariant"] },
            submission::ErrorCase { literal: "idempotency_conflict", required: &["field"], sources: &["same_key_different_canonical_command"] },
            submission::ErrorCase { literal: "internal_error", required: &[], sources: &["query_error", "row_limit_exceeded", "undeclared_constraint"] },
            submission::ErrorCase { literal: "invalid_input", required: &["field"], sources: &["envelope_count", "malformed_input"] },
            submission::ErrorCase { literal: "pallet_not_found", required: &["field", "id"], sources: &["transaction_invariant"] },
            submission::ErrorCase { literal: "permission_denied", required: &["operation"], sources: &["permission_denied"] },
            submission::ErrorCase { literal: "quantity_not_found", required: &["field", "id"], sources: &["transaction_invariant"] },
            submission::ErrorCase { literal: "retry", required: &[], sources: &["connection_unavailable", "serialization_failure"] },
            submission::ErrorCase { literal: "timeout", required: &[], sources: &["statement_timeout"] },
        ],
        kind: "command",
        transaction: Some("explicit_per_input"),
        direct: true,
        replay: submission::Replay::Claim,
    },
    route: Some(crate::inventory::adjust_route),
    record: None,
    revision: None,
    revision_inputs: &["value.expected_row_version"],
    requires_composition: true,
    supplied: &[
        screen::SuppliedField { path: "request_id", kind: screen::SuppliedKind::RequestId },
        screen::SuppliedField { path: "value.idempotency_key", kind: screen::SuppliedKind::IdempotencyKey },
        screen::SuppliedField { path: "value.occurred_at", kind: screen::SuppliedKind::OccurredAt },
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
    kind: "projection",
    input: crate::inventory::INVENTORY_AGGREGATE_INPUT_SCHEMA,
    input_schema: Some("{\"items\":{\"additionalProperties\":false,\"properties\":{\"request_id\":{\"minLength\":1,\"type\":\"string\"}},\"required\":[\"request_id\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}"),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        fields: crate::inventory::INVENTORY_AGGREGATE_RESULT_SCHEMA,
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
    route: Some(crate::inventory::aggregate_route),
    record: None,
    revision: None,
    revision_inputs: &[],
    requires_composition: false,
    supplied: &[
        screen::SuppliedField { path: "request_id", kind: screen::SuppliedKind::RequestId },
    ],
};

#[must_use]
pub fn aggregate(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&AGGREGATE_SPEC, binding)
}

pub static MERGE_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "inventory",
    name: "merge",
    operation: "wamn-wms:inventory/merge@1.0.0",
    kind: "command",
    input: crate::inventory::INVENTORY_MERGE_INPUT_SCHEMA,
    input_schema: Some("{\"items\":{\"additionalProperties\":false,\"properties\":{\"request_id\":{\"minLength\":1,\"type\":\"string\"},\"value\":{\"additionalProperties\":false,\"properties\":{\"expected_row_version\":{\"minimum\":1,\"type\":\"integer\"},\"idempotency_key\":{\"minLength\":1,\"type\":\"string\"},\"occurred_at\":{\"format\":\"date-time\",\"type\":\"string\"},\"source_pallet_id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"},\"target_pallet_id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"}},\"required\":[\"idempotency_key\",\"source_pallet_id\",\"target_pallet_id\",\"expected_row_version\",\"occurred_at\"],\"type\":\"object\"}},\"required\":[\"request_id\",\"value\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}"),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        fields: crate::inventory::INVENTORY_MERGE_RESULT_SCHEMA,
        result_class: Some("one"),
        errors: &[
            submission::ErrorCase { literal: "concurrency_conflict", required: &["expected_row_version", "observed_row_version"], sources: &["transaction_invariant"] },
            submission::ErrorCase { literal: "idempotency_conflict", required: &["field"], sources: &["same_key_different_canonical_command"] },
            submission::ErrorCase { literal: "internal_error", required: &[], sources: &["query_error", "row_limit_exceeded", "undeclared_constraint"] },
            submission::ErrorCase { literal: "invalid_input", required: &["field"], sources: &["envelope_count", "malformed_input"] },
            submission::ErrorCase { literal: "pallet_not_found", required: &["field", "id"], sources: &["transaction_invariant"] },
            submission::ErrorCase { literal: "permission_denied", required: &["operation"], sources: &["permission_denied"] },
            submission::ErrorCase { literal: "retry", required: &[], sources: &["connection_unavailable", "serialization_failure"] },
            submission::ErrorCase { literal: "timeout", required: &[], sources: &["statement_timeout"] },
        ],
        kind: "command",
        transaction: Some("explicit_per_input"),
        direct: true,
        replay: submission::Replay::Claim,
    },
    route: Some(crate::inventory::merge_route),
    record: None,
    revision: None,
    revision_inputs: &["value.expected_row_version"],
    requires_composition: true,
    supplied: &[
        screen::SuppliedField { path: "request_id", kind: screen::SuppliedKind::RequestId },
        screen::SuppliedField { path: "value.idempotency_key", kind: screen::SuppliedKind::IdempotencyKey },
        screen::SuppliedField { path: "value.occurred_at", kind: screen::SuppliedKind::OccurredAt },
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
    kind: "command",
    input: crate::inventory::INVENTORY_MOVE_INPUT_SCHEMA,
    input_schema: Some("{\"items\":{\"additionalProperties\":false,\"properties\":{\"request_id\":{\"minLength\":1,\"type\":\"string\"},\"value\":{\"additionalProperties\":false,\"properties\":{\"expected_row_version\":{\"minimum\":1,\"type\":\"integer\"},\"idempotency_key\":{\"minLength\":1,\"type\":\"string\"},\"occurred_at\":{\"format\":\"date-time\",\"type\":\"string\"},\"pallet_id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"},\"to_location_id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"}},\"required\":[\"idempotency_key\",\"pallet_id\",\"to_location_id\",\"expected_row_version\",\"occurred_at\"],\"type\":\"object\"}},\"required\":[\"request_id\",\"value\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}"),
    response: submission::ResponseContract {
        schema: None,
        fields: crate::inventory::INVENTORY_MOVE_RESULT_SCHEMA,
        result_class: None,
        errors: &[
        ],
        kind: "command",
        transaction: Some("explicit_per_input"),
        direct: false,
        replay: submission::Replay::Unknown,
    },
    route: Some(crate::inventory::move_route),
    record: None,
    revision: None,
    revision_inputs: &["value.expected_row_version"],
    requires_composition: true,
    supplied: &[
        screen::SuppliedField { path: "request_id", kind: screen::SuppliedKind::RequestId },
        screen::SuppliedField { path: "value.idempotency_key", kind: screen::SuppliedKind::IdempotencyKey },
        screen::SuppliedField { path: "value.occurred_at", kind: screen::SuppliedKind::OccurredAt },
    ],
};

#[must_use]
pub fn r#move(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&MOVE_SPEC, binding)
}

pub static SPLIT_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "inventory",
    name: "split",
    operation: "wamn-wms:inventory/split@1.0.0",
    kind: "command",
    input: crate::inventory::INVENTORY_SPLIT_INPUT_SCHEMA,
    input_schema: Some("{\"items\":{\"additionalProperties\":false,\"properties\":{\"request_id\":{\"minLength\":1,\"type\":\"string\"},\"value\":{\"additionalProperties\":false,\"properties\":{\"expected_row_version\":{\"minimum\":1,\"type\":\"integer\"},\"idempotency_key\":{\"minLength\":1,\"type\":\"string\"},\"new_pallet_code\":{\"minLength\":1,\"type\":\"string\"},\"occurred_at\":{\"format\":\"date-time\",\"type\":\"string\"},\"product_id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"},\"quantity\":{\"pattern\":\"^[0-9]+(\\\\.[0-9]+)?$\",\"type\":\"string\"},\"source_pallet_id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"},\"status\":{\"enum\":[\"available\",\"held\"],\"type\":\"string\"},\"to_location_id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"}},\"required\":[\"idempotency_key\",\"source_pallet_id\",\"product_id\",\"status\",\"quantity\",\"new_pallet_code\",\"to_location_id\",\"expected_row_version\",\"occurred_at\"],\"type\":\"object\"}},\"required\":[\"request_id\",\"value\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}"),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        fields: crate::inventory::INVENTORY_SPLIT_RESULT_SCHEMA,
        result_class: Some("one"),
        errors: &[
            submission::ErrorCase { literal: "concurrency_conflict", required: &["expected_row_version", "observed_row_version"], sources: &["transaction_invariant"] },
            submission::ErrorCase { literal: "idempotency_conflict", required: &["field"], sources: &["same_key_different_canonical_command"] },
            submission::ErrorCase { literal: "insufficient_quantity", required: &["field"], sources: &["transaction_invariant"] },
            submission::ErrorCase { literal: "internal_error", required: &[], sources: &["query_error", "row_limit_exceeded", "undeclared_constraint"] },
            submission::ErrorCase { literal: "invalid_input", required: &["field"], sources: &["envelope_count", "malformed_input"] },
            submission::ErrorCase { literal: "location_not_found", required: &["field", "id"], sources: &["transaction_invariant"] },
            submission::ErrorCase { literal: "pallet_not_found", required: &["field", "id"], sources: &["transaction_invariant"] },
            submission::ErrorCase { literal: "permission_denied", required: &["operation"], sources: &["permission_denied"] },
            submission::ErrorCase { literal: "quantity_not_found", required: &["field", "id"], sources: &["transaction_invariant"] },
            submission::ErrorCase { literal: "retry", required: &[], sources: &["connection_unavailable", "serialization_failure"] },
            submission::ErrorCase { literal: "timeout", required: &[], sources: &["statement_timeout"] },
        ],
        kind: "command",
        transaction: Some("explicit_per_input"),
        direct: true,
        replay: submission::Replay::Claim,
    },
    route: Some(crate::inventory::split_route),
    record: None,
    revision: None,
    revision_inputs: &["value.expected_row_version"],
    requires_composition: true,
    supplied: &[
        screen::SuppliedField { path: "request_id", kind: screen::SuppliedKind::RequestId },
        screen::SuppliedField { path: "value.idempotency_key", kind: screen::SuppliedKind::IdempotencyKey },
        screen::SuppliedField { path: "value.occurred_at", kind: screen::SuppliedKind::OccurredAt },
    ],
};

#[must_use]
pub fn split(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&SPLIT_SPEC, binding)
}
