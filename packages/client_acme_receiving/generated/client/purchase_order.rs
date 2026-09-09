// @generated from the client-contract IR; do not edit.
//!
//! `purchase_order` operations of package `client_acme_receiving`.

use wamn_client::{ClientError, FieldDescriptor, RouteMetadata, WamnClient};

/// Every field the `purchase_order` model projects.
pub const PURCHASE_ORDER_FIELDS: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "acme_inspection_required",
        type_name: "boolean",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "acme_quality_status",
        type_name: "text",
        nullable: false,
        values: &[
            "approved",
            "not_required",
            "pending",
        ],
    },
    FieldDescriptor {
        path: "created_at",
        type_name: "timestamptz",
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
        path: "purchase_order_number",
        type_name: "text",
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
        path: "status",
        type_name: "text",
        nullable: false,
        values: &[
            "cancelled",
            "complete",
            "open",
        ],
    },
    FieldDescriptor {
        path: "supplier_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "updated_at",
        type_name: "timestamptz",
        nullable: false,
        values: &[],
    },
];


/// Input for `client-acme-receiving:purchase-order/get@3.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct PurchaseOrderGetRequest {
    /// `uuid`
    pub id: uuid::Uuid,
    /// `string`
    pub request_id: String,
}

/// Result of `client-acme-receiving:purchase-order/get@3.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct PurchaseOrderGetResult {
    /// `boolean`
    pub acme_inspection_required: bool,
    /// `text`
    pub acme_quality_status: String,
    /// `timestamptz`
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `text`
    pub purchase_order_number: String,
    /// `int64`
    pub row_version: i64,
    /// `text`
    pub status: String,
    /// `uuid`
    pub supplier_id: uuid::Uuid,
    /// `timestamptz`
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Input descriptors for `client-acme-receiving:purchase-order/get@3.0.0`.
pub const PURCHASE_ORDER_GET_INPUT: &[FieldDescriptor] = &[
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

/// Result descriptors for `client-acme-receiving:purchase-order/get@3.0.0`.
pub const PURCHASE_ORDER_GET_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "acme_inspection_required",
        type_name: "boolean",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "acme_quality_status",
        type_name: "text",
        nullable: false,
        values: &[
            "approved",
            "not_required",
            "pending",
        ],
    },
    FieldDescriptor {
        path: "created_at",
        type_name: "timestamptz",
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
        path: "purchase_order_number",
        type_name: "text",
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
        path: "status",
        type_name: "text",
        nullable: false,
        values: &[
            "cancelled",
            "complete",
            "open",
        ],
    },
    FieldDescriptor {
        path: "supplier_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "updated_at",
        type_name: "timestamptz",
        nullable: false,
        values: &[],
    },
];

pub const PURCHASE_ORDER_GET_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "id", type_name: "uuid", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "request_id", type_name: "string", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
];

pub const PURCHASE_ORDER_GET_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "acme_inspection_required", type_name: "boolean", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "acme_quality_status", type_name: "text", nullable: false, values: &["approved", "not_required", "pending"] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "created_at", type_name: "timestamptz", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "id", type_name: "uuid", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "purchase_order_number", type_name: "text", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "row_version", type_name: "int64", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "status", type_name: "text", nullable: false, values: &["cancelled", "complete", "open"] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "supplier_id", type_name: "uuid", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "updated_at", type_name: "timestamptz", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
];

pub const PURCHASE_ORDER_GET_KIND: &str = "get";
pub const PURCHASE_ORDER_GET_REQUIRES_COMPOSITION: bool = false;
pub const PURCHASE_ORDER_GET_REPLAY: Option<&str> = None;
pub const PURCHASE_ORDER_GET_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const PURCHASE_ORDER_GET_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `client-acme-receiving:purchase-order/get@3.0.0`.
pub const PURCHASE_ORDER_GET_GRANT: &str = "client-acme-receiving:purchase-order/get@3.0.0";

/// Typed refusals `client-acme-receiving:purchase-order/get@3.0.0` declares.
pub const PURCHASE_ORDER_GET_ERRORS: &[&str] = &[
    "internal_error",
    "invalid_input",
    "not_found",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `client-acme-receiving:purchase-order/get@3.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn get_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/acme/purchase_order/get".to_owned(),
    }
}

/// Invoke `client-acme-receiving:purchase-order/get@3.0.0` through a bound client.
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


/// Input for `client-acme-receiving:purchase-order/update@3.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct PurchaseOrderUpdateRequest {
    /// `object`
    pub change: PurchaseOrderUpdateRequestChange,
    /// `int64`
    pub expected_row_version: i64,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `string`
    pub request_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PurchaseOrderUpdateRequestChange {
    /// `boolean`, omittable
    pub acme_inspection_required: Option<bool>,
    /// `text`, omittable
    pub acme_quality_status: Option<String>,
}

/// Result of `client-acme-receiving:purchase-order/update@3.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct PurchaseOrderUpdateResult {
    /// `boolean`
    pub acme_inspection_required: bool,
    /// `text`
    pub acme_quality_status: String,
    /// `timestamptz`
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `text`
    pub purchase_order_number: String,
    /// `int64`
    pub row_version: i64,
    /// `text`
    pub status: String,
    /// `uuid`
    pub supplier_id: uuid::Uuid,
    /// `timestamptz`
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Input descriptors for `client-acme-receiving:purchase-order/update@3.0.0`.
pub const PURCHASE_ORDER_UPDATE_INPUT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "change.acme_inspection_required",
        type_name: "boolean",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "change.acme_quality_status",
        type_name: "text",
        nullable: true,
        values: &[
            "approved",
            "not_required",
            "pending",
        ],
    },
    FieldDescriptor {
        path: "expected_row_version",
        type_name: "int64",
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
        path: "request_id",
        type_name: "string",
        nullable: false,
        values: &[],
    },
];

/// Result descriptors for `client-acme-receiving:purchase-order/update@3.0.0`.
pub const PURCHASE_ORDER_UPDATE_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "acme_inspection_required",
        type_name: "boolean",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "acme_quality_status",
        type_name: "text",
        nullable: false,
        values: &[
            "approved",
            "not_required",
            "pending",
        ],
    },
    FieldDescriptor {
        path: "created_at",
        type_name: "timestamptz",
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
        path: "purchase_order_number",
        type_name: "text",
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
        path: "status",
        type_name: "text",
        nullable: false,
        values: &[
            "cancelled",
            "complete",
            "open",
        ],
    },
    FieldDescriptor {
        path: "supplier_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "updated_at",
        type_name: "timestamptz",
        nullable: false,
        values: &[],
    },
];

pub const PURCHASE_ORDER_UPDATE_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "change", type_name: "object", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "change.acme_inspection_required", type_name: "boolean", nullable: false, values: &[] },
required: false, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "change.acme_quality_status", type_name: "text", nullable: false, values: &["approved", "not_required", "pending"] },
required: false, minimum: None, maximum: None, children: &[
], },
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "expected_row_version", type_name: "int64", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "id", type_name: "uuid", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "request_id", type_name: "string", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
];

pub const PURCHASE_ORDER_UPDATE_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "acme_inspection_required", type_name: "boolean", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "acme_quality_status", type_name: "text", nullable: false, values: &["approved", "not_required", "pending"] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "created_at", type_name: "timestamptz", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "id", type_name: "uuid", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "purchase_order_number", type_name: "text", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "row_version", type_name: "int64", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "status", type_name: "text", nullable: false, values: &["cancelled", "complete", "open"] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "supplier_id", type_name: "uuid", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "updated_at", type_name: "timestamptz", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
];

pub const PURCHASE_ORDER_UPDATE_KIND: &str = "update";
pub const PURCHASE_ORDER_UPDATE_REQUIRES_COMPOSITION: bool = false;
pub const PURCHASE_ORDER_UPDATE_REPLAY: Option<&str> = None;
pub const PURCHASE_ORDER_UPDATE_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const PURCHASE_ORDER_UPDATE_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `client-acme-receiving:purchase-order/update@3.0.0`.
pub const PURCHASE_ORDER_UPDATE_GRANT: &str = "client-acme-receiving:purchase-order/update@3.0.0";

/// Typed refusals `client-acme-receiving:purchase-order/update@3.0.0` declares.
pub const PURCHASE_ORDER_UPDATE_ERRORS: &[&str] = &[
    "concurrency_conflict",
    "internal_error",
    "invalid_input",
    "not_found",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `client-acme-receiving:purchase-order/update@3.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn update_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/acme/purchase_order/update".to_owned(),
    }
}

/// Invoke `client-acme-receiving:purchase-order/update@3.0.0` through a bound client.
///
/// # Errors
///
/// [`ClientError`] for a transport failure, a refusal, or a response that
/// does not match the operation's envelope.
pub async fn update(
    client: &WamnClient,
    items: &[serde_json::Value],
) -> Result<Vec<wamn_client::ItemOutcome>, ClientError> {
    client
        .invoke(&update_route(), &std::collections::BTreeMap::new(), items)
        .await
}
