// @generated from the client-contract IR; do not edit.
//!
//! `inventory` operations of package `wamn_wms`.

use wamn_client::{ClientError, FieldDescriptor, RouteMetadata, WamnClient};

/// Every field the `inventory` model projects.
pub const INVENTORY_FIELDS: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "adjusted_quantity",
        type_name: "numeric",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "location_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "movement_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "new_pallet_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "pallet_count",
        type_name: "int64",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "pallet_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "pallet_status",
        type_name: "text",
        nullable: false,
        values: &[
            "available",
            "consumed",
            "held",
        ],
    },
    FieldDescriptor {
        path: "product_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "quantity",
        type_name: "numeric",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "row_version",
        type_name: "int64",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "source_pallet_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "source_status",
        type_name: "text",
        nullable: false,
        values: &[
            "available",
            "consumed",
            "held",
        ],
    },
    FieldDescriptor {
        path: "status",
        type_name: "text",
        nullable: false,
        values: &[
            "available",
            "held",
        ],
    },
    FieldDescriptor {
        path: "target_pallet_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "target_status",
        type_name: "text",
        nullable: false,
        values: &[
            "available",
            "consumed",
            "held",
        ],
    },
];


/// Input for `wamn-wms:inventory/adjust@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct InventoryAdjustRequest {
    /// `text`
    pub request_id: String,
}

/// Result of `wamn-wms:inventory/adjust@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct InventoryAdjustResult {
    /// `numeric`
    pub adjusted_quantity: rust_decimal::Decimal,
    /// `uuid`
    pub movement_id: uuid::Uuid,
    /// `uuid`
    pub pallet_id: uuid::Uuid,
    /// `text`
    pub pallet_status: String,
    /// `int64`
    pub row_version: i64,
}

/// Input descriptors for `wamn-wms:inventory/adjust@1.0.0`.
pub const INVENTORY_ADJUST_INPUT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "request_id",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.expected_row_version",
        type_name: "int64",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.idempotency_key",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.occurred_at",
        type_name: "timestamptz",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.pallet_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.product_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.quantity",
        type_name: "numeric",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.reason_code",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.status",
        type_name: "text",
        nullable: false,
        values: &[
            "available",
            "held",
        ],
    },
];

/// Result descriptors for `wamn-wms:inventory/adjust@1.0.0`.
pub const INVENTORY_ADJUST_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "adjusted_quantity",
        type_name: "numeric",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "movement_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "pallet_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "pallet_status",
        type_name: "text",
        nullable: false,
        values: &[
            "available",
            "consumed",
            "held",
        ],
    },
    FieldDescriptor {
        path: "row_version",
        type_name: "int64",
        nullable: false,
        values: &[],
    },
];

/// The grant a caller presents to invoke `wamn-wms:inventory/adjust@1.0.0`.
pub const INVENTORY_ADJUST_GRANT: &str = "wamn-wms:inventory/adjust@1.0.0";

/// Typed refusals `wamn-wms:inventory/adjust@1.0.0` declares.
pub const INVENTORY_ADJUST_ERRORS: &[&str] = &[
    "concurrency_conflict",
    "idempotency_conflict",
    "internal_error",
    "invalid_input",
    "pallet_not_found",
    "permission_denied",
    "quantity_not_found",
    "retry",
    "timeout",
];

/// Where the release publishes `wamn-wms:inventory/adjust@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn adjust_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/inventory/adjust".to_owned(),
    }
}

/// Invoke `wamn-wms:inventory/adjust@1.0.0` through a bound client.
///
/// # Errors
///
/// [`ClientError`] for a transport failure, a refusal, or a response that
/// does not match the operation's envelope.
pub async fn adjust(
    client: &WamnClient,
    items: &[serde_json::Value],
) -> Result<Vec<wamn_client::ItemOutcome>, ClientError> {
    client
        .invoke(&adjust_route(), &std::collections::BTreeMap::new(), items)
        .await
}


/// Input for `wamn-wms:inventory/aggregate@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct InventoryAggregateRequest {
    /// `text`
    pub request_id: String,
}

/// Result of `wamn-wms:inventory/aggregate@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct InventoryAggregateResult {
    /// `uuid`
    pub location_id: uuid::Uuid,
    /// `int64`
    pub pallet_count: i64,
    /// `uuid`
    pub product_id: uuid::Uuid,
    /// `numeric`
    pub quantity: rust_decimal::Decimal,
    /// `text`
    pub status: String,
}

/// Input descriptors for `wamn-wms:inventory/aggregate@1.0.0`.
pub const INVENTORY_AGGREGATE_INPUT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "request_id",
        type_name: "text",
        nullable: false,
        values: &[],
    },
];

/// Result descriptors for `wamn-wms:inventory/aggregate@1.0.0`.
pub const INVENTORY_AGGREGATE_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "location_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "pallet_count",
        type_name: "int64",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "product_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "quantity",
        type_name: "numeric",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "status",
        type_name: "text",
        nullable: false,
        values: &[
            "available",
            "held",
        ],
    },
];

/// The grant a caller presents to invoke `wamn-wms:inventory/aggregate@1.0.0`.
pub const INVENTORY_AGGREGATE_GRANT: &str = "wamn-wms:inventory/aggregate@1.0.0";

/// Typed refusals `wamn-wms:inventory/aggregate@1.0.0` declares.
pub const INVENTORY_AGGREGATE_ERRORS: &[&str] = &[
    "internal_error",
    "invalid_input",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `wamn-wms:inventory/aggregate@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn aggregate_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/inventory/aggregate".to_owned(),
    }
}

/// Invoke `wamn-wms:inventory/aggregate@1.0.0` through a bound client.
///
/// # Errors
///
/// [`ClientError`] for a transport failure, a refusal, or a response that
/// does not match the operation's envelope.
pub async fn aggregate(
    client: &WamnClient,
    items: &[serde_json::Value],
) -> Result<Vec<wamn_client::ItemOutcome>, ClientError> {
    client
        .invoke(&aggregate_route(), &std::collections::BTreeMap::new(), items)
        .await
}


/// Input for `wamn-wms:inventory/merge@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct InventoryMergeRequest {
    /// `text`
    pub request_id: String,
}

/// Result of `wamn-wms:inventory/merge@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct InventoryMergeResult {
    /// `uuid`
    pub movement_id: uuid::Uuid,
    /// `int64`
    pub row_version: i64,
    /// `uuid`
    pub source_pallet_id: uuid::Uuid,
    /// `uuid`
    pub target_pallet_id: uuid::Uuid,
    /// `text`
    pub target_status: String,
}

/// Input descriptors for `wamn-wms:inventory/merge@1.0.0`.
pub const INVENTORY_MERGE_INPUT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "request_id",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.expected_row_version",
        type_name: "int64",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.idempotency_key",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.occurred_at",
        type_name: "timestamptz",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.source_pallet_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.target_pallet_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
];

/// Result descriptors for `wamn-wms:inventory/merge@1.0.0`.
pub const INVENTORY_MERGE_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "movement_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "row_version",
        type_name: "int64",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "source_pallet_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "target_pallet_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "target_status",
        type_name: "text",
        nullable: false,
        values: &[
            "available",
            "consumed",
            "held",
        ],
    },
];

/// The grant a caller presents to invoke `wamn-wms:inventory/merge@1.0.0`.
pub const INVENTORY_MERGE_GRANT: &str = "wamn-wms:inventory/merge@1.0.0";

/// Typed refusals `wamn-wms:inventory/merge@1.0.0` declares.
pub const INVENTORY_MERGE_ERRORS: &[&str] = &[
    "concurrency_conflict",
    "idempotency_conflict",
    "internal_error",
    "invalid_input",
    "pallet_not_found",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `wamn-wms:inventory/merge@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn merge_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/inventory/merge".to_owned(),
    }
}

/// Invoke `wamn-wms:inventory/merge@1.0.0` through a bound client.
///
/// # Errors
///
/// [`ClientError`] for a transport failure, a refusal, or a response that
/// does not match the operation's envelope.
pub async fn merge(
    client: &WamnClient,
    items: &[serde_json::Value],
) -> Result<Vec<wamn_client::ItemOutcome>, ClientError> {
    client
        .invoke(&merge_route(), &std::collections::BTreeMap::new(), items)
        .await
}


/// Input for `wamn-wms:inventory/move@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct InventoryMoveRequest {
    /// `text`
    pub request_id: String,
}

/// Result of `wamn-wms:inventory/move@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct InventoryMoveResult {
    /// `uuid`
    pub location_id: uuid::Uuid,
    /// `uuid`
    pub movement_id: uuid::Uuid,
    /// `uuid`
    pub pallet_id: uuid::Uuid,
    /// `text`
    pub pallet_status: String,
    /// `int64`
    pub row_version: i64,
}

/// Input descriptors for `wamn-wms:inventory/move@1.0.0`.
pub const INVENTORY_MOVE_INPUT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "request_id",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.expected_row_version",
        type_name: "int64",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.idempotency_key",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.occurred_at",
        type_name: "timestamptz",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.pallet_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.to_location_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
];

/// Result descriptors for `wamn-wms:inventory/move@1.0.0`.
pub const INVENTORY_MOVE_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "location_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "movement_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "pallet_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "pallet_status",
        type_name: "text",
        nullable: false,
        values: &[
            "available",
            "consumed",
            "held",
        ],
    },
    FieldDescriptor {
        path: "row_version",
        type_name: "int64",
        nullable: false,
        values: &[],
    },
];

/// The grant a caller presents to invoke `wamn-wms:inventory/move@1.0.0`.
pub const INVENTORY_MOVE_GRANT: &str = "wamn-wms:inventory/move@1.0.0";

/// Typed refusals `wamn-wms:inventory/move@1.0.0` declares.
pub const INVENTORY_MOVE_ERRORS: &[&str] = &[
    "concurrency_conflict",
    "idempotency_conflict",
    "internal_error",
    "invalid_input",
    "location_not_found",
    "pallet_not_found",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `wamn-wms:inventory/move@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn move_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/inventory/move".to_owned(),
    }
}

/// Invoke `wamn-wms:inventory/move@1.0.0` through a bound client.
///
/// # Errors
///
/// [`ClientError`] for a transport failure, a refusal, or a response that
/// does not match the operation's envelope.
pub async fn r#move(
    client: &WamnClient,
    items: &[serde_json::Value],
) -> Result<Vec<wamn_client::ItemOutcome>, ClientError> {
    client
        .invoke(&move_route(), &std::collections::BTreeMap::new(), items)
        .await
}


/// Input for `wamn-wms:inventory/split@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct InventorySplitRequest {
    /// `text`
    pub request_id: String,
}

/// Result of `wamn-wms:inventory/split@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct InventorySplitResult {
    /// `uuid`
    pub movement_id: uuid::Uuid,
    /// `uuid`
    pub new_pallet_id: uuid::Uuid,
    /// `int64`
    pub row_version: i64,
    /// `uuid`
    pub source_pallet_id: uuid::Uuid,
    /// `text`
    pub source_status: String,
}

/// Input descriptors for `wamn-wms:inventory/split@1.0.0`.
pub const INVENTORY_SPLIT_INPUT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "request_id",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.expected_row_version",
        type_name: "int64",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.idempotency_key",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.new_pallet_code",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.occurred_at",
        type_name: "timestamptz",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.product_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.quantity",
        type_name: "numeric",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.source_pallet_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.status",
        type_name: "text",
        nullable: false,
        values: &[
            "available",
            "held",
        ],
    },
    FieldDescriptor {
        path: "value.to_location_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
];

/// Result descriptors for `wamn-wms:inventory/split@1.0.0`.
pub const INVENTORY_SPLIT_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "movement_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "new_pallet_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "row_version",
        type_name: "int64",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "source_pallet_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "source_status",
        type_name: "text",
        nullable: false,
        values: &[
            "available",
            "consumed",
            "held",
        ],
    },
];

/// The grant a caller presents to invoke `wamn-wms:inventory/split@1.0.0`.
pub const INVENTORY_SPLIT_GRANT: &str = "wamn-wms:inventory/split@1.0.0";

/// Typed refusals `wamn-wms:inventory/split@1.0.0` declares.
pub const INVENTORY_SPLIT_ERRORS: &[&str] = &[
    "concurrency_conflict",
    "idempotency_conflict",
    "insufficient_quantity",
    "internal_error",
    "invalid_input",
    "location_not_found",
    "pallet_not_found",
    "permission_denied",
    "quantity_not_found",
    "retry",
    "timeout",
];

/// Where the release publishes `wamn-wms:inventory/split@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn split_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/inventory/split".to_owned(),
    }
}

/// Invoke `wamn-wms:inventory/split@1.0.0` through a bound client.
///
/// # Errors
///
/// [`ClientError`] for a transport failure, a refusal, or a response that
/// does not match the operation's envelope.
pub async fn split(
    client: &WamnClient,
    items: &[serde_json::Value],
) -> Result<Vec<wamn_client::ItemOutcome>, ClientError> {
    client
        .invoke(&split_route(), &std::collections::BTreeMap::new(), items)
        .await
}
