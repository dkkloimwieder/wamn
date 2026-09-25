// @generated; do not edit.

use wamn_client_tui::{screen, submission};

pub static ADJUST_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "inventory",
    name: "adjust",
    operation: "wamn-wms:inventory/adjust@1.0.0",
    kind: "command",
    input: crate::inventory::INVENTORY_ADJUST_INPUT_SCHEMA,
    input_schema: Some(
        "{\"items\":{\"additionalProperties\":false,\"properties\":{\"request_id\":{\"minLength\":1,\"type\":\"string\"},\"value\":{\"additionalProperties\":false,\"properties\":{\"expected_row_version\":{\"type\":\"integer\"},\"idempotency_key\":{\"minLength\":1,\"type\":\"string\"},\"inventory_id\":{\"format\":\"uuid\",\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"},\"occurred_at\":{\"format\":\"date-time\",\"type\":\"string\"},\"reason\":{\"minLength\":1,\"type\":\"string\"},\"to_quantity\":{\"pattern\":\"^[0-9]+(\\\\.[0-9]+)?$\",\"type\":\"string\"}},\"required\":[\"idempotency_key\",\"inventory_id\",\"to_quantity\",\"reason\",\"expected_row_version\",\"occurred_at\"],\"type\":\"object\"}},\"required\":[\"request_id\",\"value\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}",
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
                literal: "not_found",
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
        kind: "command",
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
            kind: screen::SuppliedKind::RequestId,
        },
        screen::SuppliedField {
            path: "value.idempotency_key",
            kind: screen::SuppliedKind::IdempotencyKey,
        },
        screen::SuppliedField {
            path: "value.occurred_at",
            kind: screen::SuppliedKind::OccurredAt,
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
    kind: "projection",
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
        kind: "projection",
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

pub static GET_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "inventory",
    name: "get",
    operation: "wamn-wms:inventory/get@1.0.0",
    kind: "get",
    input: crate::inventory::INVENTORY_GET_INPUT_SCHEMA,
    input_schema: Some(
        "{\"items\":{\"additionalProperties\":false,\"properties\":{\"id\":{\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"}},\"required\":[\"id\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}",
    ),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::inventory::INVENTORY_GET_RESULT_SCHEMA,
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
    route: Some(crate::inventory::get_route),
    fresh_only: false,
    record: Some(screen::RecordLink {
        relation: "wms.inventory",
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

pub static MERGE_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "inventory",
    name: "merge",
    operation: "wamn-wms:inventory/merge@1.0.0",
    kind: "command",
    input: crate::inventory::INVENTORY_MERGE_INPUT_SCHEMA,
    input_schema: Some(
        "{\"items\":{\"additionalProperties\":false,\"properties\":{\"request_id\":{\"minLength\":1,\"type\":\"string\"},\"value\":{\"additionalProperties\":false,\"properties\":{\"expected_from_row_version\":{\"type\":\"integer\"},\"expected_to_row_version\":{\"type\":\"integer\"},\"from_inventory_id\":{\"format\":\"uuid\",\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"},\"idempotency_key\":{\"minLength\":1,\"type\":\"string\"},\"occurred_at\":{\"format\":\"date-time\",\"type\":\"string\"},\"to_inventory_id\":{\"format\":\"uuid\",\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"}},\"required\":[\"idempotency_key\",\"from_inventory_id\",\"to_inventory_id\",\"expected_from_row_version\",\"expected_to_row_version\",\"occurred_at\"],\"type\":\"object\"}},\"required\":[\"request_id\",\"value\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}",
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
                literal: "not_found",
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
        kind: "command",
        transaction: Some("explicit_per_input"),
        direct: true,
        replay: submission::Replay::Claim,
    },
    route: Some(crate::inventory::merge_route),
    fresh_only: false,
    record: None,
    revision: None,
    revision_inputs: &[
        "value.expected_from_row_version",
        "value.expected_to_row_version",
    ],
    requires_composition: true,
    supplied: &[
        screen::SuppliedField {
            path: "request_id",
            kind: screen::SuppliedKind::RequestId,
        },
        screen::SuppliedField {
            path: "value.idempotency_key",
            kind: screen::SuppliedKind::IdempotencyKey,
        },
        screen::SuppliedField {
            path: "value.occurred_at",
            kind: screen::SuppliedKind::OccurredAt,
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
    kind: "command",
    input: crate::inventory::INVENTORY_MOVE_INPUT_SCHEMA,
    input_schema: Some(
        "{\"items\":{\"additionalProperties\":false,\"properties\":{\"request_id\":{\"minLength\":1,\"type\":\"string\"},\"value\":{\"additionalProperties\":false,\"properties\":{\"expected_row_version\":{\"type\":\"integer\"},\"idempotency_key\":{\"minLength\":1,\"type\":\"string\"},\"inventory_id\":{\"format\":\"uuid\",\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"},\"occurred_at\":{\"format\":\"date-time\",\"type\":\"string\"},\"to_location_id\":{\"format\":\"uuid\",\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"},\"to_packaging_id\":{\"format\":\"uuid\",\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"}},\"required\":[\"idempotency_key\",\"inventory_id\",\"to_packaging_id\",\"to_location_id\",\"expected_row_version\",\"occurred_at\"],\"type\":\"object\"}},\"required\":[\"request_id\",\"value\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}",
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
                literal: "not_found",
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
        kind: "command",
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
            kind: screen::SuppliedKind::RequestId,
        },
        screen::SuppliedField {
            path: "value.idempotency_key",
            kind: screen::SuppliedKind::IdempotencyKey,
        },
        screen::SuppliedField {
            path: "value.occurred_at",
            kind: screen::SuppliedKind::OccurredAt,
        },
    ],
};

#[must_use]
pub fn r#move(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&MOVE_SPEC, binding)
}

pub static QUERY_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "inventory",
    name: "query",
    operation: "wamn-wms:inventory/query@1.0.0",
    kind: "query",
    input: crate::inventory::INVENTORY_QUERY_INPUT_SCHEMA,
    input_schema: Some(
        "{\"items\":{\"additionalProperties\":false,\"properties\":{\"cursor\":{\"minLength\":1,\"type\":\"string\"},\"limit\":{\"maximum\":100,\"minimum\":1,\"type\":\"integer\"}},\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}",
    ),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::inventory::INVENTORY_QUERY_RESULT_SCHEMA,
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
    route: Some(crate::inventory::query_route),
    fresh_only: false,
    record: Some(screen::RecordLink {
        relation: "wms.inventory",
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

pub static SPLIT_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "inventory",
    name: "split",
    operation: "wamn-wms:inventory/split@1.0.0",
    kind: "command",
    input: crate::inventory::INVENTORY_SPLIT_INPUT_SCHEMA,
    input_schema: Some(
        "{\"items\":{\"additionalProperties\":false,\"properties\":{\"request_id\":{\"minLength\":1,\"type\":\"string\"},\"value\":{\"additionalProperties\":false,\"properties\":{\"expected_row_version\":{\"type\":\"integer\"},\"from_inventory_id\":{\"format\":\"uuid\",\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"},\"idempotency_key\":{\"minLength\":1,\"type\":\"string\"},\"occurred_at\":{\"format\":\"date-time\",\"type\":\"string\"},\"quantity\":{\"pattern\":\"^[0-9]+(\\\\.[0-9]+)?$\",\"type\":\"string\"},\"to_location_id\":{\"format\":\"uuid\",\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"},\"to_packaging_id\":{\"format\":\"uuid\",\"pattern\":\"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$\",\"type\":\"string\"}},\"required\":[\"idempotency_key\",\"from_inventory_id\",\"quantity\",\"to_packaging_id\",\"to_location_id\",\"expected_row_version\",\"occurred_at\"],\"type\":\"object\"}},\"required\":[\"request_id\",\"value\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}",
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
                sources: &["same_key_different_canonical_command"],
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
                literal: "not_found",
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
        kind: "command",
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
            kind: screen::SuppliedKind::RequestId,
        },
        screen::SuppliedField {
            path: "value.idempotency_key",
            kind: screen::SuppliedKind::IdempotencyKey,
        },
        screen::SuppliedField {
            path: "value.occurred_at",
            kind: screen::SuppliedKind::OccurredAt,
        },
    ],
};

#[must_use]
pub fn split(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&SPLIT_SPEC, binding)
}
