// @generated from the client-contract IR; do not edit.
//!
//! `inventory_movement` operations of package `wamn_wms`.

use wamn_client::{ClientError, FieldDescriptor, RouteMetadata, WamnClient};

/// Every field the `inventory_movement` model projects.
pub const INVENTORY_MOVEMENT_FIELDS: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "created_at",
        type_name: "timestamptz",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "created_by",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "from_location_id",
        type_name: "uuid",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "idempotency_key",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "kind",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "occurred_at",
        type_name: "timestamptz",
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
        path: "reason_code",
        type_name: "text",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "to_location_id",
        type_name: "uuid",
        nullable: true,
        values: &[],
    },
];

/// Input for `wamn-wms:inventory-movement/get@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct InventoryMovementGetRequest {
    /// `uuid`
    pub id: uuid::Uuid,
    /// `string`
    pub request_id: String,
}

/// Result of `wamn-wms:inventory-movement/get@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct InventoryMovementGetResult {
    /// `timestamptz`
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// `uuid`
    pub created_by: uuid::Uuid,
    /// `uuid`
    pub from_location_id: Option<uuid::Uuid>,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `text`
    pub idempotency_key: String,
    /// `text`
    pub kind: String,
    /// `timestamptz`
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    /// `uuid`
    pub pallet_id: uuid::Uuid,
    /// `uuid`
    pub product_id: uuid::Uuid,
    /// `numeric`
    pub quantity: rust_decimal::Decimal,
    /// `text`
    pub reason_code: Option<String>,
    /// `uuid`
    pub to_location_id: Option<uuid::Uuid>,
}

/// Input descriptors for `wamn-wms:inventory-movement/get@1.0.0`.
pub const INVENTORY_MOVEMENT_GET_INPUT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "request_id",
        type_name: "string",
        nullable: false,
        values: &[],
    },
];

/// Result descriptors for `wamn-wms:inventory-movement/get@1.0.0`.
pub const INVENTORY_MOVEMENT_GET_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "created_at",
        type_name: "timestamptz",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "created_by",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "from_location_id",
        type_name: "uuid",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "idempotency_key",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "kind",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "occurred_at",
        type_name: "timestamptz",
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
        path: "reason_code",
        type_name: "text",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "to_location_id",
        type_name: "uuid",
        nullable: true,
        values: &[],
    },
];

pub const INVENTORY_MOVEMENT_GET_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
            path: "request_id",
            type_name: "string",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
];

pub const INVENTORY_MOVEMENT_GET_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
            path: "created_by",
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
            path: "from_location_id",
            type_name: "uuid",
            nullable: true,
            values: &[],
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
            path: "idempotency_key",
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
            path: "kind",
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
            path: "occurred_at",
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
            path: "pallet_id",
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
            path: "reason_code",
            type_name: "text",
            nullable: true,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "to_location_id",
            type_name: "uuid",
            nullable: true,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
];

pub const INVENTORY_MOVEMENT_GET_KIND: &str = "get";
pub const INVENTORY_MOVEMENT_GET_REQUIRES_COMPOSITION: bool = false;
pub const INVENTORY_MOVEMENT_GET_REPLAY: Option<&str> = None;
pub const INVENTORY_MOVEMENT_GET_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const INVENTORY_MOVEMENT_GET_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `wamn-wms:inventory-movement/get@1.0.0`.
pub const INVENTORY_MOVEMENT_GET_GRANT: &str = "wamn-wms:inventory-movement/get@1.0.0";

/// Typed refusals `wamn-wms:inventory-movement/get@1.0.0` declares.
pub const INVENTORY_MOVEMENT_GET_ERRORS: &[&str] = &[
    "internal_error",
    "invalid_input",
    "not_found",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `wamn-wms:inventory-movement/get@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn get_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/inventory_movement/get".to_owned(),
    }
}

/// Invoke `wamn-wms:inventory-movement/get@1.0.0` through a bound client.
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

/// Input for `wamn-wms:inventory-movement/query@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct InventoryMovementQueryRequest {
    /// `text`, omittable
    pub cursor: Option<String>,
    /// `int32`, omittable
    pub limit: Option<i32>,
    /// `string`
    pub request_id: String,
}

/// Result of `wamn-wms:inventory-movement/query@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct InventoryMovementQueryResult {
    /// `timestamptz`
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// `uuid`
    pub created_by: uuid::Uuid,
    /// `uuid`
    pub from_location_id: Option<uuid::Uuid>,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `text`
    pub idempotency_key: String,
    /// `text`
    pub kind: String,
    /// `timestamptz`
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    /// `uuid`
    pub pallet_id: uuid::Uuid,
    /// `uuid`
    pub product_id: uuid::Uuid,
    /// `numeric`
    pub quantity: rust_decimal::Decimal,
    /// `text`
    pub reason_code: Option<String>,
    /// `uuid`
    pub to_location_id: Option<uuid::Uuid>,
}

/// Input descriptors for `wamn-wms:inventory-movement/query@1.0.0`.
pub const INVENTORY_MOVEMENT_QUERY_INPUT: &[FieldDescriptor] = &[
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
    FieldDescriptor {
        path: "request_id",
        type_name: "string",
        nullable: false,
        values: &[],
    },
];

/// Result descriptors for `wamn-wms:inventory-movement/query@1.0.0`.
pub const INVENTORY_MOVEMENT_QUERY_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "created_at",
        type_name: "timestamptz",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "created_by",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "from_location_id",
        type_name: "uuid",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "idempotency_key",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "kind",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "occurred_at",
        type_name: "timestamptz",
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
        path: "reason_code",
        type_name: "text",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "to_location_id",
        type_name: "uuid",
        nullable: true,
        values: &[],
    },
];

pub const INVENTORY_MOVEMENT_QUERY_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "request_id",
            type_name: "string",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
];

pub const INVENTORY_MOVEMENT_QUERY_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
            path: "created_by",
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
            path: "from_location_id",
            type_name: "uuid",
            nullable: true,
            values: &[],
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
            path: "idempotency_key",
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
            path: "kind",
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
            path: "occurred_at",
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
            path: "pallet_id",
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
            path: "reason_code",
            type_name: "text",
            nullable: true,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "to_location_id",
            type_name: "uuid",
            nullable: true,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
];

pub const INVENTORY_MOVEMENT_QUERY_KIND: &str = "query";
pub const INVENTORY_MOVEMENT_QUERY_REQUIRES_COMPOSITION: bool = false;
pub const INVENTORY_MOVEMENT_QUERY_REPLAY: Option<&str> = None;
pub const INVENTORY_MOVEMENT_QUERY_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const INVENTORY_MOVEMENT_QUERY_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `wamn-wms:inventory-movement/query@1.0.0`.
pub const INVENTORY_MOVEMENT_QUERY_GRANT: &str = "wamn-wms:inventory-movement/query@1.0.0";

/// Typed refusals `wamn-wms:inventory-movement/query@1.0.0` declares.
pub const INVENTORY_MOVEMENT_QUERY_ERRORS: &[&str] = &[
    "internal_error",
    "invalid_input",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `wamn-wms:inventory-movement/query@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn query_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/inventory_movement/query".to_owned(),
    }
}

/// Invoke `wamn-wms:inventory-movement/query@1.0.0` through a bound client.
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
