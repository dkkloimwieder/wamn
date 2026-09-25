// @generated from the client-contract IR; do not edit.
//!
//! `inventory` operations of package `wamn_wms`.

use wamn_client::{ClientError, FieldDescriptor, RouteMetadata, WamnClient};

/// Every field the `inventory` model projects.
pub const INVENTORY_FIELDS: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "created_at",
        type_name: "timestamptz",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "disposition",
        type_name: "text",
        nullable: false,
        values: &["available", "held"],
    },
    FieldDescriptor {
        path: "from_inventory_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "inventory_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "lifecycle",
        type_name: "text",
        nullable: false,
        values: &["closed", "open"],
    },
    FieldDescriptor {
        path: "location_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "new_inventory_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "operation_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "packaging_count",
        type_name: "int32",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "packaging_id",
        type_name: "uuid",
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
        path: "row_version",
        type_name: "int32",
        nullable: false,
        values: &[],
    },
];

/// Input for `wamn-wms:inventory/adjust@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct InventoryAdjustRequest {
    /// `text`
    pub request_id: String,
    /// `object`
    pub value: InventoryAdjustRequestValue,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InventoryAdjustRequestValue {
    /// `int32`
    pub expected_row_version: i32,
    /// `text`
    pub idempotency_key: String,
    /// `uuid`
    pub inventory_id: uuid::Uuid,
    /// `timestamptz`
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    /// `text`
    pub reason: String,
    /// `numeric`
    pub to_quantity: rust_decimal::Decimal,
}

/// Result of `wamn-wms:inventory/adjust@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct InventoryAdjustResult {
    /// `text`
    pub disposition: String,
    /// `uuid`
    pub inventory_id: uuid::Uuid,
    /// `text`
    pub lifecycle: String,
    /// `uuid`
    pub location_id: uuid::Uuid,
    /// `uuid`
    pub operation_id: uuid::Uuid,
    /// `uuid`
    pub packaging_id: uuid::Uuid,
    /// `uuid`
    pub product_id: uuid::Uuid,
    /// `numeric`
    pub quantity: rust_decimal::Decimal,
    /// `int32`
    pub row_version: i32,
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
        type_name: "int32",
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
        path: "value.inventory_id",
        type_name: "uuid",
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
        path: "value.reason",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.to_quantity",
        type_name: "numeric",
        nullable: false,
        values: &[],
    },
];

/// Result descriptors for `wamn-wms:inventory/adjust@1.0.0`.
pub const INVENTORY_ADJUST_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "disposition",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "inventory_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "lifecycle",
        type_name: "text",
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
        path: "operation_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "packaging_id",
        type_name: "uuid",
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
        path: "row_version",
        type_name: "int32",
        nullable: false,
        values: &[],
    },
];

pub const INVENTORY_ADJUST_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "request_id",
            type_name: "text",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "value",
            type_name: "object",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "value.expected_row_version",
                    type_name: "int32",
                    nullable: false,
                    values: &[],
                },
                required: true,
                minimum: None,
                maximum: None,
                children: &[],
            },
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "value.idempotency_key",
                    type_name: "text",
                    nullable: false,
                    values: &[],
                },
                required: true,
                minimum: None,
                maximum: None,
                children: &[],
            },
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "value.inventory_id",
                    type_name: "uuid",
                    nullable: false,
                    values: &[],
                },
                required: true,
                minimum: None,
                maximum: None,
                children: &[],
            },
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "value.occurred_at",
                    type_name: "timestamptz",
                    nullable: false,
                    values: &[],
                },
                required: true,
                minimum: None,
                maximum: None,
                children: &[],
            },
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "value.reason",
                    type_name: "text",
                    nullable: false,
                    values: &[],
                },
                required: true,
                minimum: None,
                maximum: None,
                children: &[],
            },
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "value.to_quantity",
                    type_name: "numeric",
                    nullable: false,
                    values: &[],
                },
                required: true,
                minimum: None,
                maximum: None,
                children: &[],
            },
        ],
    },
];

pub const INVENTORY_ADJUST_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "disposition",
            type_name: "text",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "inventory_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "lifecycle",
            type_name: "text",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "location_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "operation_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "packaging_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "product_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "quantity",
            type_name: "numeric",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "row_version",
            type_name: "int32",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
];

pub const INVENTORY_ADJUST_KIND: &str = "command";
pub const INVENTORY_ADJUST_REQUIRES_COMPOSITION: bool = true;
pub const INVENTORY_ADJUST_REPLAY: Option<&str> = Some("claim");
pub const INVENTORY_ADJUST_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const INVENTORY_ADJUST_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `wamn-wms:inventory/adjust@1.0.0`.
pub const INVENTORY_ADJUST_GRANT: &str = "wamn-wms:inventory/adjust@1.0.0";

/// Typed refusals `wamn-wms:inventory/adjust@1.0.0` declares.
pub const INVENTORY_ADJUST_ERRORS: &[&str] = &[
    "concurrency_conflict",
    "idempotency_conflict",
    "internal_error",
    "invalid_input",
    "not_found",
    "permission_denied",
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
pub struct InventoryAggregateRequest {}

/// Result of `wamn-wms:inventory/aggregate@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct InventoryAggregateResult {
    /// `text`
    pub disposition: String,
    /// `uuid`
    pub location_id: uuid::Uuid,
    /// `int32`
    pub packaging_count: i32,
    /// `uuid`
    pub product_id: uuid::Uuid,
    /// `numeric`
    pub quantity: rust_decimal::Decimal,
}

/// Input descriptors for `wamn-wms:inventory/aggregate@1.0.0`.
pub const INVENTORY_AGGREGATE_INPUT: &[FieldDescriptor] = &[];

/// Result descriptors for `wamn-wms:inventory/aggregate@1.0.0`.
pub const INVENTORY_AGGREGATE_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "disposition",
        type_name: "text",
        nullable: false,
        values: &["available", "held"],
    },
    FieldDescriptor {
        path: "location_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "packaging_count",
        type_name: "int32",
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
];

pub const INVENTORY_AGGREGATE_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[];

pub const INVENTORY_AGGREGATE_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "disposition",
            type_name: "text",
            nullable: false,
            values: &["available", "held"],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "location_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "packaging_count",
            type_name: "int32",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "product_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "quantity",
            type_name: "numeric",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
];

pub const INVENTORY_AGGREGATE_KIND: &str = "projection";
pub const INVENTORY_AGGREGATE_REQUIRES_COMPOSITION: bool = false;
pub const INVENTORY_AGGREGATE_REPLAY: Option<&str> = None;
pub const INVENTORY_AGGREGATE_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const INVENTORY_AGGREGATE_RESULT_OPAQUE: bool = false;
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
        method: "GET".to_owned(),
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
        .invoke(
            &aggregate_route(),
            &std::collections::BTreeMap::new(),
            items,
        )
        .await
}

/// Input for `wamn-wms:inventory/get@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct InventoryGetRequest {
    /// `uuid`
    pub id: uuid::Uuid,
}

/// Result of `wamn-wms:inventory/get@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct InventoryGetResult {
    /// `timestamptz`
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// `text`
    pub disposition: String,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `text`
    pub lifecycle: String,
    /// `uuid`
    pub location_id: uuid::Uuid,
    /// `uuid`
    pub packaging_id: uuid::Uuid,
    /// `uuid`
    pub product_id: uuid::Uuid,
    /// `numeric`
    pub quantity: rust_decimal::Decimal,
    /// `int32`
    pub row_version: i32,
}

/// Input descriptors for `wamn-wms:inventory/get@1.0.0`.
pub const INVENTORY_GET_INPUT: &[FieldDescriptor] = &[FieldDescriptor {
    path: "id",
    type_name: "uuid",
    nullable: false,
    values: &[],
}];

/// Result descriptors for `wamn-wms:inventory/get@1.0.0`.
pub const INVENTORY_GET_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "created_at",
        type_name: "timestamptz",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "disposition",
        type_name: "text",
        nullable: false,
        values: &["available", "held"],
    },
    FieldDescriptor {
        path: "id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "lifecycle",
        type_name: "text",
        nullable: false,
        values: &["closed", "open"],
    },
    FieldDescriptor {
        path: "location_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "packaging_id",
        type_name: "uuid",
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
        path: "row_version",
        type_name: "int32",
        nullable: false,
        values: &[],
    },
];

pub const INVENTORY_GET_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] =
    &[wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    }];

pub const INVENTORY_GET_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "created_at",
            type_name: "timestamptz",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "disposition",
            type_name: "text",
            nullable: false,
            values: &["available", "held"],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "lifecycle",
            type_name: "text",
            nullable: false,
            values: &["closed", "open"],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "location_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "packaging_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "product_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "quantity",
            type_name: "numeric",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "row_version",
            type_name: "int32",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
];

pub const INVENTORY_GET_KIND: &str = "get";
pub const INVENTORY_GET_REQUIRES_COMPOSITION: bool = false;
pub const INVENTORY_GET_REPLAY: Option<&str> = None;
pub const INVENTORY_GET_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const INVENTORY_GET_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `wamn-wms:inventory/get@1.0.0`.
pub const INVENTORY_GET_GRANT: &str = "wamn-wms:inventory/get@1.0.0";

/// Typed refusals `wamn-wms:inventory/get@1.0.0` declares.
pub const INVENTORY_GET_ERRORS: &[&str] = &[
    "internal_error",
    "invalid_input",
    "not_found",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `wamn-wms:inventory/get@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn get_route() -> RouteMetadata {
    RouteMetadata {
        method: "GET".to_owned(),
        template: "/inventory/get".to_owned(),
    }
}

/// Invoke `wamn-wms:inventory/get@1.0.0` through a bound client.
///
/// # Errors
///
/// [`ClientError`] for a transport failure, a refusal, or a response that
/// does not match the operation's envelope.
pub async fn get(
    client: &WamnClient,
    items: &[serde_json::Value],
) -> Result<Vec<wamn_client::ItemOutcome>, ClientError> {
    client
        .invoke(&get_route(), &std::collections::BTreeMap::new(), items)
        .await
}

/// Input for `wamn-wms:inventory/merge@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct InventoryMergeRequest {
    /// `text`
    pub request_id: String,
    /// `object`
    pub value: InventoryMergeRequestValue,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InventoryMergeRequestValue {
    /// `int32`
    pub expected_from_row_version: i32,
    /// `int32`
    pub expected_to_row_version: i32,
    /// `uuid`
    pub from_inventory_id: uuid::Uuid,
    /// `text`
    pub idempotency_key: String,
    /// `timestamptz`
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    /// `uuid`
    pub to_inventory_id: uuid::Uuid,
}

/// Result of `wamn-wms:inventory/merge@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct InventoryMergeResult {
    /// `text`
    pub disposition: String,
    /// `uuid`
    pub from_inventory_id: uuid::Uuid,
    /// `uuid`
    pub inventory_id: uuid::Uuid,
    /// `text`
    pub lifecycle: String,
    /// `uuid`
    pub location_id: uuid::Uuid,
    /// `uuid`
    pub operation_id: uuid::Uuid,
    /// `uuid`
    pub packaging_id: uuid::Uuid,
    /// `uuid`
    pub product_id: uuid::Uuid,
    /// `numeric`
    pub quantity: rust_decimal::Decimal,
    /// `int32`
    pub row_version: i32,
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
        path: "value.expected_from_row_version",
        type_name: "int32",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.expected_to_row_version",
        type_name: "int32",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.from_inventory_id",
        type_name: "uuid",
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
        path: "value.to_inventory_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
];

/// Result descriptors for `wamn-wms:inventory/merge@1.0.0`.
pub const INVENTORY_MERGE_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "disposition",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "from_inventory_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "inventory_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "lifecycle",
        type_name: "text",
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
        path: "operation_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "packaging_id",
        type_name: "uuid",
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
        path: "row_version",
        type_name: "int32",
        nullable: false,
        values: &[],
    },
];

pub const INVENTORY_MERGE_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "request_id",
            type_name: "text",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "value",
            type_name: "object",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "value.expected_from_row_version",
                    type_name: "int32",
                    nullable: false,
                    values: &[],
                },
                required: true,
                minimum: None,
                maximum: None,
                children: &[],
            },
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "value.expected_to_row_version",
                    type_name: "int32",
                    nullable: false,
                    values: &[],
                },
                required: true,
                minimum: None,
                maximum: None,
                children: &[],
            },
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "value.from_inventory_id",
                    type_name: "uuid",
                    nullable: false,
                    values: &[],
                },
                required: true,
                minimum: None,
                maximum: None,
                children: &[],
            },
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "value.idempotency_key",
                    type_name: "text",
                    nullable: false,
                    values: &[],
                },
                required: true,
                minimum: None,
                maximum: None,
                children: &[],
            },
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "value.occurred_at",
                    type_name: "timestamptz",
                    nullable: false,
                    values: &[],
                },
                required: true,
                minimum: None,
                maximum: None,
                children: &[],
            },
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "value.to_inventory_id",
                    type_name: "uuid",
                    nullable: false,
                    values: &[],
                },
                required: true,
                minimum: None,
                maximum: None,
                children: &[],
            },
        ],
    },
];

pub const INVENTORY_MERGE_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "disposition",
            type_name: "text",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "from_inventory_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "inventory_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "lifecycle",
            type_name: "text",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "location_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "operation_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "packaging_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "product_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "quantity",
            type_name: "numeric",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "row_version",
            type_name: "int32",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
];

pub const INVENTORY_MERGE_KIND: &str = "command";
pub const INVENTORY_MERGE_REQUIRES_COMPOSITION: bool = true;
pub const INVENTORY_MERGE_REPLAY: Option<&str> = Some("claim");
pub const INVENTORY_MERGE_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const INVENTORY_MERGE_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `wamn-wms:inventory/merge@1.0.0`.
pub const INVENTORY_MERGE_GRANT: &str = "wamn-wms:inventory/merge@1.0.0";

/// Typed refusals `wamn-wms:inventory/merge@1.0.0` declares.
pub const INVENTORY_MERGE_ERRORS: &[&str] = &[
    "concurrency_conflict",
    "idempotency_conflict",
    "internal_error",
    "invalid_input",
    "not_found",
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
    /// `object`
    pub value: InventoryMoveRequestValue,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InventoryMoveRequestValue {
    /// `int32`
    pub expected_row_version: i32,
    /// `text`
    pub idempotency_key: String,
    /// `uuid`
    pub inventory_id: uuid::Uuid,
    /// `timestamptz`
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    /// `uuid`
    pub to_location_id: uuid::Uuid,
    /// `uuid`
    pub to_packaging_id: uuid::Uuid,
}

/// Result of `wamn-wms:inventory/move@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct InventoryMoveResult {
    /// `text`
    pub disposition: String,
    /// `uuid`
    pub inventory_id: uuid::Uuid,
    /// `text`
    pub lifecycle: String,
    /// `uuid`
    pub location_id: uuid::Uuid,
    /// `uuid`
    pub operation_id: uuid::Uuid,
    /// `uuid`
    pub packaging_id: uuid::Uuid,
    /// `uuid`
    pub product_id: uuid::Uuid,
    /// `numeric`
    pub quantity: rust_decimal::Decimal,
    /// `int32`
    pub row_version: i32,
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
        type_name: "int32",
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
        path: "value.inventory_id",
        type_name: "uuid",
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
        path: "value.to_location_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.to_packaging_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
];

/// Result descriptors for `wamn-wms:inventory/move@1.0.0`.
pub const INVENTORY_MOVE_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "disposition",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "inventory_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "lifecycle",
        type_name: "text",
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
        path: "operation_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "packaging_id",
        type_name: "uuid",
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
        path: "row_version",
        type_name: "int32",
        nullable: false,
        values: &[],
    },
];

pub const INVENTORY_MOVE_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "request_id",
            type_name: "text",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "value",
            type_name: "object",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "value.expected_row_version",
                    type_name: "int32",
                    nullable: false,
                    values: &[],
                },
                required: true,
                minimum: None,
                maximum: None,
                children: &[],
            },
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "value.idempotency_key",
                    type_name: "text",
                    nullable: false,
                    values: &[],
                },
                required: true,
                minimum: None,
                maximum: None,
                children: &[],
            },
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "value.inventory_id",
                    type_name: "uuid",
                    nullable: false,
                    values: &[],
                },
                required: true,
                minimum: None,
                maximum: None,
                children: &[],
            },
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "value.occurred_at",
                    type_name: "timestamptz",
                    nullable: false,
                    values: &[],
                },
                required: true,
                minimum: None,
                maximum: None,
                children: &[],
            },
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "value.to_location_id",
                    type_name: "uuid",
                    nullable: false,
                    values: &[],
                },
                required: true,
                minimum: None,
                maximum: None,
                children: &[],
            },
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "value.to_packaging_id",
                    type_name: "uuid",
                    nullable: false,
                    values: &[],
                },
                required: true,
                minimum: None,
                maximum: None,
                children: &[],
            },
        ],
    },
];

pub const INVENTORY_MOVE_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "disposition",
            type_name: "text",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "inventory_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "lifecycle",
            type_name: "text",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "location_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "operation_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "packaging_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "product_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "quantity",
            type_name: "numeric",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "row_version",
            type_name: "int32",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
];

pub const INVENTORY_MOVE_KIND: &str = "command";
pub const INVENTORY_MOVE_REQUIRES_COMPOSITION: bool = true;
pub const INVENTORY_MOVE_REPLAY: Option<&str> = Some("claim");
pub const INVENTORY_MOVE_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const INVENTORY_MOVE_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `wamn-wms:inventory/move@1.0.0`.
pub const INVENTORY_MOVE_GRANT: &str = "wamn-wms:inventory/move@1.0.0";

/// Typed refusals `wamn-wms:inventory/move@1.0.0` declares.
pub const INVENTORY_MOVE_ERRORS: &[&str] = &[
    "concurrency_conflict",
    "idempotency_conflict",
    "internal_error",
    "invalid_input",
    "not_found",
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

/// Input for `wamn-wms:inventory/query@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct InventoryQueryRequest {
    /// `text`, omittable
    pub cursor: Option<String>,
    /// `int32`, omittable
    pub limit: Option<i32>,
}

/// Result of `wamn-wms:inventory/query@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct InventoryQueryResult {
    /// `timestamptz`
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// `text`
    pub disposition: String,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `text`
    pub lifecycle: String,
    /// `uuid`
    pub location_id: uuid::Uuid,
    /// `uuid`
    pub packaging_id: uuid::Uuid,
    /// `uuid`
    pub product_id: uuid::Uuid,
    /// `numeric`
    pub quantity: rust_decimal::Decimal,
    /// `int32`
    pub row_version: i32,
}

/// Input descriptors for `wamn-wms:inventory/query@1.0.0`.
pub const INVENTORY_QUERY_INPUT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "cursor",
        type_name: "text",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "limit",
        type_name: "int32",
        nullable: true,
        values: &[],
    },
];

/// Result descriptors for `wamn-wms:inventory/query@1.0.0`.
pub const INVENTORY_QUERY_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "created_at",
        type_name: "timestamptz",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "disposition",
        type_name: "text",
        nullable: false,
        values: &["available", "held"],
    },
    FieldDescriptor {
        path: "id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "lifecycle",
        type_name: "text",
        nullable: false,
        values: &["closed", "open"],
    },
    FieldDescriptor {
        path: "location_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "packaging_id",
        type_name: "uuid",
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
        path: "row_version",
        type_name: "int32",
        nullable: false,
        values: &[],
    },
];

pub const INVENTORY_QUERY_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "cursor",
            type_name: "text",
            nullable: false,
            values: &[],
        },
        required: false,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "limit",
            type_name: "int32",
            nullable: false,
            values: &[],
        },
        required: false,
        minimum: None,
        maximum: None,
        children: &[],
    },
];

pub const INVENTORY_QUERY_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "created_at",
            type_name: "timestamptz",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "disposition",
            type_name: "text",
            nullable: false,
            values: &["available", "held"],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "lifecycle",
            type_name: "text",
            nullable: false,
            values: &["closed", "open"],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "location_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "packaging_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "product_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "quantity",
            type_name: "numeric",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "row_version",
            type_name: "int32",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
];

pub const INVENTORY_QUERY_KIND: &str = "query";
pub const INVENTORY_QUERY_REQUIRES_COMPOSITION: bool = false;
pub const INVENTORY_QUERY_REPLAY: Option<&str> = None;
pub const INVENTORY_QUERY_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const INVENTORY_QUERY_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `wamn-wms:inventory/query@1.0.0`.
pub const INVENTORY_QUERY_GRANT: &str = "wamn-wms:inventory/query@1.0.0";

/// Typed refusals `wamn-wms:inventory/query@1.0.0` declares.
pub const INVENTORY_QUERY_ERRORS: &[&str] = &[
    "internal_error",
    "invalid_input",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `wamn-wms:inventory/query@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn query_route() -> RouteMetadata {
    RouteMetadata {
        method: "GET".to_owned(),
        template: "/inventory/query".to_owned(),
    }
}

/// Invoke `wamn-wms:inventory/query@1.0.0` through a bound client.
///
/// # Errors
///
/// [`ClientError`] for a transport failure, a refusal, or a response that
/// does not match the operation's envelope.
pub async fn query(
    client: &WamnClient,
    items: &[serde_json::Value],
) -> Result<Vec<wamn_client::ItemOutcome>, ClientError> {
    client
        .invoke(&query_route(), &std::collections::BTreeMap::new(), items)
        .await
}

/// Input for `wamn-wms:inventory/split@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct InventorySplitRequest {
    /// `text`
    pub request_id: String,
    /// `object`
    pub value: InventorySplitRequestValue,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InventorySplitRequestValue {
    /// `int32`
    pub expected_row_version: i32,
    /// `uuid`
    pub from_inventory_id: uuid::Uuid,
    /// `text`
    pub idempotency_key: String,
    /// `timestamptz`
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    /// `numeric`
    pub quantity: rust_decimal::Decimal,
    /// `uuid`
    pub to_location_id: uuid::Uuid,
    /// `uuid`
    pub to_packaging_id: uuid::Uuid,
}

/// Result of `wamn-wms:inventory/split@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct InventorySplitResult {
    /// `text`
    pub disposition: String,
    /// `uuid`
    pub inventory_id: uuid::Uuid,
    /// `text`
    pub lifecycle: String,
    /// `uuid`
    pub location_id: uuid::Uuid,
    /// `uuid`
    pub new_inventory_id: uuid::Uuid,
    /// `uuid`
    pub operation_id: uuid::Uuid,
    /// `uuid`
    pub packaging_id: uuid::Uuid,
    /// `uuid`
    pub product_id: uuid::Uuid,
    /// `numeric`
    pub quantity: rust_decimal::Decimal,
    /// `int32`
    pub row_version: i32,
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
        type_name: "int32",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.from_inventory_id",
        type_name: "uuid",
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
        path: "value.quantity",
        type_name: "numeric",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.to_location_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.to_packaging_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
];

/// Result descriptors for `wamn-wms:inventory/split@1.0.0`.
pub const INVENTORY_SPLIT_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "disposition",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "inventory_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "lifecycle",
        type_name: "text",
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
        path: "new_inventory_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "operation_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "packaging_id",
        type_name: "uuid",
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
        path: "row_version",
        type_name: "int32",
        nullable: false,
        values: &[],
    },
];

pub const INVENTORY_SPLIT_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "request_id",
            type_name: "text",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "value",
            type_name: "object",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "value.expected_row_version",
                    type_name: "int32",
                    nullable: false,
                    values: &[],
                },
                required: true,
                minimum: None,
                maximum: None,
                children: &[],
            },
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "value.from_inventory_id",
                    type_name: "uuid",
                    nullable: false,
                    values: &[],
                },
                required: true,
                minimum: None,
                maximum: None,
                children: &[],
            },
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "value.idempotency_key",
                    type_name: "text",
                    nullable: false,
                    values: &[],
                },
                required: true,
                minimum: None,
                maximum: None,
                children: &[],
            },
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "value.occurred_at",
                    type_name: "timestamptz",
                    nullable: false,
                    values: &[],
                },
                required: true,
                minimum: None,
                maximum: None,
                children: &[],
            },
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "value.quantity",
                    type_name: "numeric",
                    nullable: false,
                    values: &[],
                },
                required: true,
                minimum: None,
                maximum: None,
                children: &[],
            },
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "value.to_location_id",
                    type_name: "uuid",
                    nullable: false,
                    values: &[],
                },
                required: true,
                minimum: None,
                maximum: None,
                children: &[],
            },
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "value.to_packaging_id",
                    type_name: "uuid",
                    nullable: false,
                    values: &[],
                },
                required: true,
                minimum: None,
                maximum: None,
                children: &[],
            },
        ],
    },
];

pub const INVENTORY_SPLIT_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "disposition",
            type_name: "text",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "inventory_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "lifecycle",
            type_name: "text",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "location_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "new_inventory_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "operation_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "packaging_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "product_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "quantity",
            type_name: "numeric",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "row_version",
            type_name: "int32",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
];

pub const INVENTORY_SPLIT_KIND: &str = "command";
pub const INVENTORY_SPLIT_REQUIRES_COMPOSITION: bool = true;
pub const INVENTORY_SPLIT_REPLAY: Option<&str> = Some("claim");
pub const INVENTORY_SPLIT_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const INVENTORY_SPLIT_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `wamn-wms:inventory/split@1.0.0`.
pub const INVENTORY_SPLIT_GRANT: &str = "wamn-wms:inventory/split@1.0.0";

/// Typed refusals `wamn-wms:inventory/split@1.0.0` declares.
pub const INVENTORY_SPLIT_ERRORS: &[&str] = &[
    "concurrency_conflict",
    "idempotency_conflict",
    "insufficient_quantity",
    "internal_error",
    "invalid_input",
    "not_found",
    "permission_denied",
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
