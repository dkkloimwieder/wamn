// @generated from the client-contract IR; do not edit.
//!
//! `inventory_transaction` operations of package `wamn_wms`.

use wamn_client::{ClientError, FieldDescriptor, RouteMetadata, WamnClient};

/// Every field the `inventory_transaction` model projects.
pub const INVENTORY_TRANSACTION_FIELDS: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "created_at",
        type_name: "timestamptz",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "from_disposition",
        type_name: "text",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "from_inventory_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "from_lifecycle",
        type_name: "text",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "from_location_id",
        type_name: "uuid",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "from_packaging_id",
        type_name: "uuid",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "from_product_id",
        type_name: "uuid",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "from_quantity",
        type_name: "numeric",
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
        path: "occurred_at",
        type_name: "timestamptz",
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
        path: "reason",
        type_name: "text",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "to_disposition",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "to_inventory_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "to_lifecycle",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "to_location_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "to_packaging_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "to_product_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "to_quantity",
        type_name: "numeric",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "type",
        type_name: "text",
        nullable: false,
        values: &[],
    },
];

/// Input for `wamn-wms:inventory-transaction/get@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct InventoryTransactionGetRequest {
    /// `uuid`
    pub id: uuid::Uuid,
}

/// Result of `wamn-wms:inventory-transaction/get@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct InventoryTransactionGetResult {
    /// `timestamptz`
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// `text`
    pub from_disposition: Option<String>,
    /// `uuid`
    pub from_inventory_id: uuid::Uuid,
    /// `text`
    pub from_lifecycle: Option<String>,
    /// `uuid`
    pub from_location_id: Option<uuid::Uuid>,
    /// `uuid`
    pub from_packaging_id: Option<uuid::Uuid>,
    /// `uuid`
    pub from_product_id: Option<uuid::Uuid>,
    /// `numeric`
    pub from_quantity: rust_decimal::Decimal,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `uuid`
    pub inventory_id: uuid::Uuid,
    /// `timestamptz`
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    /// `uuid`
    pub operation_id: uuid::Uuid,
    /// `text`
    pub reason: Option<String>,
    /// `text`
    pub to_disposition: String,
    /// `uuid`
    pub to_inventory_id: uuid::Uuid,
    /// `text`
    pub to_lifecycle: String,
    /// `uuid`
    pub to_location_id: uuid::Uuid,
    /// `uuid`
    pub to_packaging_id: uuid::Uuid,
    /// `uuid`
    pub to_product_id: uuid::Uuid,
    /// `numeric`
    pub to_quantity: rust_decimal::Decimal,
    /// `text`
    pub r#type: String,
}

/// Input descriptors for `wamn-wms:inventory-transaction/get@1.0.0`.
pub const INVENTORY_TRANSACTION_GET_INPUT: &[FieldDescriptor] = &[FieldDescriptor {
    path: "id",
    type_name: "uuid",
    nullable: false,
    values: &[],
}];

/// Result descriptors for `wamn-wms:inventory-transaction/get@1.0.0`.
pub const INVENTORY_TRANSACTION_GET_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "created_at",
        type_name: "timestamptz",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "from_disposition",
        type_name: "text",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "from_inventory_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "from_lifecycle",
        type_name: "text",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "from_location_id",
        type_name: "uuid",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "from_packaging_id",
        type_name: "uuid",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "from_product_id",
        type_name: "uuid",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "from_quantity",
        type_name: "numeric",
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
        path: "occurred_at",
        type_name: "timestamptz",
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
        path: "reason",
        type_name: "text",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "to_disposition",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "to_inventory_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "to_lifecycle",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "to_location_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "to_packaging_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "to_product_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "to_quantity",
        type_name: "numeric",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "type",
        type_name: "text",
        nullable: false,
        values: &[],
    },
];

pub const INVENTORY_TRANSACTION_GET_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] =
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

pub const INVENTORY_TRANSACTION_GET_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
            path: "from_disposition",
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
            path: "from_lifecycle",
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
            path: "from_packaging_id",
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
            path: "from_product_id",
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
            path: "from_quantity",
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
            path: "reason",
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
            path: "to_disposition",
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
            path: "to_inventory_id",
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
            path: "to_lifecycle",
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
            path: "to_location_id",
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
            path: "to_packaging_id",
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
            path: "to_product_id",
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
            path: "to_quantity",
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
            path: "type",
            type_name: "text",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
];

pub const INVENTORY_TRANSACTION_GET_KIND: &str = "get";
pub const INVENTORY_TRANSACTION_GET_REQUIRES_COMPOSITION: bool = false;
pub const INVENTORY_TRANSACTION_GET_REPLAY: Option<&str> = None;
pub const INVENTORY_TRANSACTION_GET_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const INVENTORY_TRANSACTION_GET_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `wamn-wms:inventory-transaction/get@1.0.0`.
pub const INVENTORY_TRANSACTION_GET_GRANT: &str = "wamn-wms:inventory-transaction/get@1.0.0";

/// Typed refusals `wamn-wms:inventory-transaction/get@1.0.0` declares.
pub const INVENTORY_TRANSACTION_GET_ERRORS: &[&str] = &[
    "internal_error",
    "invalid_input",
    "not_found",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `wamn-wms:inventory-transaction/get@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn get_route() -> RouteMetadata {
    RouteMetadata {
        method: "GET".to_owned(),
        template: "/inventory_transaction/get".to_owned(),
    }
}

/// Invoke `wamn-wms:inventory-transaction/get@1.0.0` through a bound client.
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

/// Input for `wamn-wms:inventory-transaction/query@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct InventoryTransactionQueryRequest {
    /// `text`, omittable
    pub cursor: Option<String>,
    /// `int32`, omittable
    pub limit: Option<i32>,
}

/// Result of `wamn-wms:inventory-transaction/query@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct InventoryTransactionQueryResult {
    /// `timestamptz`
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// `text`
    pub from_disposition: Option<String>,
    /// `uuid`
    pub from_inventory_id: uuid::Uuid,
    /// `text`
    pub from_lifecycle: Option<String>,
    /// `uuid`
    pub from_location_id: Option<uuid::Uuid>,
    /// `uuid`
    pub from_packaging_id: Option<uuid::Uuid>,
    /// `uuid`
    pub from_product_id: Option<uuid::Uuid>,
    /// `numeric`
    pub from_quantity: rust_decimal::Decimal,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `uuid`
    pub inventory_id: uuid::Uuid,
    /// `timestamptz`
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    /// `uuid`
    pub operation_id: uuid::Uuid,
    /// `text`
    pub reason: Option<String>,
    /// `text`
    pub to_disposition: String,
    /// `uuid`
    pub to_inventory_id: uuid::Uuid,
    /// `text`
    pub to_lifecycle: String,
    /// `uuid`
    pub to_location_id: uuid::Uuid,
    /// `uuid`
    pub to_packaging_id: uuid::Uuid,
    /// `uuid`
    pub to_product_id: uuid::Uuid,
    /// `numeric`
    pub to_quantity: rust_decimal::Decimal,
    /// `text`
    pub r#type: String,
}

/// Input descriptors for `wamn-wms:inventory-transaction/query@1.0.0`.
pub const INVENTORY_TRANSACTION_QUERY_INPUT: &[FieldDescriptor] = &[
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

/// Result descriptors for `wamn-wms:inventory-transaction/query@1.0.0`.
pub const INVENTORY_TRANSACTION_QUERY_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "created_at",
        type_name: "timestamptz",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "from_disposition",
        type_name: "text",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "from_inventory_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "from_lifecycle",
        type_name: "text",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "from_location_id",
        type_name: "uuid",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "from_packaging_id",
        type_name: "uuid",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "from_product_id",
        type_name: "uuid",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "from_quantity",
        type_name: "numeric",
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
        path: "occurred_at",
        type_name: "timestamptz",
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
        path: "reason",
        type_name: "text",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "to_disposition",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "to_inventory_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "to_lifecycle",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "to_location_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "to_packaging_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "to_product_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "to_quantity",
        type_name: "numeric",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "type",
        type_name: "text",
        nullable: false,
        values: &[],
    },
];

pub const INVENTORY_TRANSACTION_QUERY_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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

pub const INVENTORY_TRANSACTION_QUERY_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
            path: "from_disposition",
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
            path: "from_lifecycle",
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
            path: "from_packaging_id",
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
            path: "from_product_id",
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
            path: "from_quantity",
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
            path: "reason",
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
            path: "to_disposition",
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
            path: "to_inventory_id",
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
            path: "to_lifecycle",
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
            path: "to_location_id",
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
            path: "to_packaging_id",
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
            path: "to_product_id",
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
            path: "to_quantity",
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
            path: "type",
            type_name: "text",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
];

pub const INVENTORY_TRANSACTION_QUERY_KIND: &str = "query";
pub const INVENTORY_TRANSACTION_QUERY_REQUIRES_COMPOSITION: bool = false;
pub const INVENTORY_TRANSACTION_QUERY_REPLAY: Option<&str> = None;
pub const INVENTORY_TRANSACTION_QUERY_RESPONSE_CONTRACT: Option<&str> =
    Some("{\"type\":\"array\"}");
pub const INVENTORY_TRANSACTION_QUERY_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `wamn-wms:inventory-transaction/query@1.0.0`.
pub const INVENTORY_TRANSACTION_QUERY_GRANT: &str = "wamn-wms:inventory-transaction/query@1.0.0";

/// Typed refusals `wamn-wms:inventory-transaction/query@1.0.0` declares.
pub const INVENTORY_TRANSACTION_QUERY_ERRORS: &[&str] = &[
    "internal_error",
    "invalid_input",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `wamn-wms:inventory-transaction/query@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn query_route() -> RouteMetadata {
    RouteMetadata {
        method: "GET".to_owned(),
        template: "/inventory_transaction/query".to_owned(),
    }
}

/// Invoke `wamn-wms:inventory-transaction/query@1.0.0` through a bound client.
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
