// @generated; do not edit.

use wamn_client_tui::{screen, submission};

pub static ARCHIVE_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "widget",
    name: "archive",
    operation: "platform-fixture:widget/archive@1.0.0",
    kind: "command",
    input: crate::widget::WIDGET_ARCHIVE_INPUT_SCHEMA,
    input_schema: None,
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::widget::WIDGET_ARCHIVE_RESULT_SCHEMA,
        result_class: Some("one"),
        errors: &[
            submission::ErrorCase {
                literal: "already_archived",
                required: &["field"],
                sources: &["transaction_invariant"],
            },
            submission::ErrorCase {
                literal: "concurrency_conflict",
                required: &["expected_row_version", "observed_row_version"],
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
        kind: "command",
        transaction: Some("explicit_per_input"),
        direct: true,
        replay: submission::Replay::State,
    },
    route: Some(crate::widget::archive_route),
    fresh_only: false,
    record: None,
    revision: None,
    revision_inputs: &["expected_edit_version"],
    requires_composition: true,
    supplied: &[],
};

#[must_use]
pub fn archive(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&ARCHIVE_SPEC, binding)
}

pub static CREATE_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "widget",
    name: "create",
    operation: "platform-fixture:widget/create@1.0.0",
    kind: "create",
    input: crate::widget::WIDGET_CREATE_INPUT_SCHEMA,
    input_schema: None,
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::widget::WIDGET_CREATE_RESULT_SCHEMA,
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
    route: Some(crate::widget::create_route),
    fresh_only: false,
    record: Some(screen::RecordLink {
        relation: "inventory.widget",
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

pub static DELETE_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "widget",
    name: "delete",
    operation: "platform-fixture:widget/delete@1.0.0",
    kind: "delete",
    input: crate::widget::WIDGET_DELETE_INPUT_SCHEMA,
    input_schema: None,
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::widget::WIDGET_DELETE_RESULT_SCHEMA,
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
        kind: "delete",
        transaction: Some("implicit"),
        direct: true,
        replay: submission::Replay::Unknown,
    },
    route: Some(crate::widget::delete_route),
    fresh_only: false,
    record: Some(screen::RecordLink {
        relation: "inventory.widget",
        key_field: "id",
        key_input: Some("id"),
    }),
    revision: Some(screen::RevisionBinding {
        read_operation: "platform-fixture:widget/get@1.0.0",
        read_key_input: "id",
        key_field: "id",
        revision_field: "edit_version",
        command_key_input: "id",
        command_revision_input: "expected_edit_version",
    }),
    revision_inputs: &["expected_edit_version"],
    requires_composition: false,
    supplied: &[screen::SuppliedField {
        path: "request_id",
        kind: screen::SuppliedKind::RequestId,
    }],
};

#[must_use]
pub fn delete(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&DELETE_SPEC, binding)
}

pub static GET_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "widget",
    name: "get",
    operation: "platform-fixture:widget/get@1.0.0",
    kind: "get",
    input: crate::widget::WIDGET_GET_INPUT_SCHEMA,
    input_schema: None,
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::widget::WIDGET_GET_RESULT_SCHEMA,
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
    route: Some(crate::widget::get_route),
    fresh_only: false,
    record: Some(screen::RecordLink {
        relation: "inventory.widget",
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

pub static LIST_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "widget",
    name: "list",
    operation: "platform-fixture:widget/list@1.0.0",
    kind: "projection",
    input: crate::widget::WIDGET_LIST_INPUT_SCHEMA,
    input_schema: None,
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::widget::WIDGET_LIST_RESULT_SCHEMA,
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
    route: Some(crate::widget::list_route),
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
    model: "widget",
    name: "query",
    operation: "platform-fixture:widget/query@1.0.0",
    kind: "query",
    input: crate::widget::WIDGET_QUERY_INPUT_SCHEMA,
    input_schema: Some(
        "{\"items\":{\"properties\":{\"cursor\":{\"type\":\"string\"},\"filter\":{\"properties\":{\"code\":{\"items\":{\"type\":\"string\"},\"type\":\"array\"}},\"type\":\"object\"},\"limit\":{\"type\":\"integer\"},\"request_id\":{\"type\":\"string\"},\"sort\":{\"properties\":{\"direction\":{\"type\":\"string\"},\"field\":{\"type\":\"string\"}},\"required\":[\"field\",\"direction\"],\"type\":\"object\"}},\"required\":[\"request_id\"],\"type\":\"object\"},\"type\":\"array\"}",
    ),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::widget::WIDGET_QUERY_RESULT_SCHEMA,
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
    route: Some(crate::widget::query_route),
    fresh_only: false,
    record: Some(screen::RecordLink {
        relation: "inventory.widget",
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

pub static RECORD_BATCH_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "widget",
    name: "record_batch",
    operation: "platform-fixture:widget/record-batch@1.0.0",
    kind: "command",
    input: crate::widget::WIDGET_RECORD_BATCH_INPUT_SCHEMA,
    input_schema: Some(
        "{\"items\":{\"additionalProperties\":false,\"properties\":{\"request_id\":{\"minLength\":1,\"type\":\"string\"},\"value\":{\"additionalProperties\":false,\"properties\":{\"expected_edit_version\":{\"type\":\"string\"},\"grade\":{\"enum\":[\"first\",\"second\"],\"type\":\"string\"},\"idempotency_key\":{\"minLength\":1,\"type\":\"string\"},\"inspector_id\":{\"format\":\"uuid\",\"type\":[\"string\",\"null\"]},\"line\":{\"items\":{\"additionalProperties\":false,\"properties\":{\"amount\":{\"type\":\"string\"},\"widget_id\":{\"format\":\"uuid\",\"type\":\"string\"}},\"required\":[\"widget_id\",\"amount\"],\"type\":\"object\"},\"maxItems\":10,\"minItems\":1,\"type\":\"array\"},\"maker_id\":{\"format\":\"uuid\",\"type\":[\"string\",\"null\"]},\"note\":{\"type\":[\"string\",\"null\"]}},\"required\":[\"idempotency_key\",\"expected_edit_version\",\"grade\",\"line\"],\"type\":\"object\"}},\"required\":[\"request_id\",\"value\"],\"type\":\"object\"},\"maxItems\":100,\"minItems\":1,\"type\":\"array\"}",
    ),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::widget::WIDGET_RECORD_BATCH_RESULT_SCHEMA,
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
                sources: &[
                    "duplicate_line",
                    "envelope_count",
                    "line_count",
                    "malformed_input",
                    "nonpositive_quantity",
                ],
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
    route: Some(crate::widget::record_batch_route),
    fresh_only: false,
    record: None,
    revision: None,
    revision_inputs: &["value.expected_edit_version"],
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
    ],
};

#[must_use]
pub fn record_batch(binding: submission::SessionBinding) -> screen::Screen {
    screen::Screen::new(&RECORD_BATCH_SPEC, binding)
}

pub static UPDATE_SPEC: screen::ScreenSpec = screen::ScreenSpec {
    model: "widget",
    name: "update",
    operation: "platform-fixture:widget/update@1.0.0",
    kind: "update",
    input: crate::widget::WIDGET_UPDATE_INPUT_SCHEMA,
    input_schema: Some(
        "{\"items\":{\"properties\":{\"change\":{\"properties\":{\"code\":{\"type\":[\"string\",\"null\"],\"x-wamn-explicit-null\":\"invalid_input\"},\"maker_id\":{\"format\":\"uuid\",\"type\":[\"string\",\"null\"],\"x-wamn-explicit-null\":\"accepted\"},\"note\":{\"type\":[\"string\",\"null\"],\"x-wamn-explicit-null\":\"accepted\"}},\"type\":\"object\"},\"expected_edit_version\":{\"type\":\"string\"},\"id\":{\"format\":\"uuid\",\"type\":\"string\"},\"request_id\":{\"type\":\"string\"}},\"required\":[\"id\",\"expected_edit_version\",\"request_id\"],\"type\":\"object\"},\"type\":\"array\"}",
    ),
    response: submission::ResponseContract {
        schema: Some("{\"type\":\"array\"}"),
        partial_schema: None,
        fields: crate::widget::WIDGET_UPDATE_RESULT_SCHEMA,
        result_class: Some("one"),
        errors: &[
            submission::ErrorCase {
                literal: "concurrency_conflict",
                required: &["expected_row_version", "observed_row_version"],
                sources: &[],
            },
            submission::ErrorCase {
                literal: "foreign_key_violation",
                required: &["constraint"],
                sources: &["foreign_key_violation"],
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
    route: Some(crate::widget::update_route),
    fresh_only: false,
    record: Some(screen::RecordLink {
        relation: "inventory.widget",
        key_field: "id",
        key_input: Some("id"),
    }),
    revision: Some(screen::RevisionBinding {
        read_operation: "platform-fixture:widget/get@1.0.0",
        read_key_input: "id",
        key_field: "id",
        revision_field: "edit_version",
        command_key_input: "id",
        command_revision_input: "expected_edit_version",
    }),
    revision_inputs: &["expected_edit_version"],
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
