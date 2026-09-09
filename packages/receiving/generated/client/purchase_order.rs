// @generated from the client-contract IR; do not edit.
//!
//! `purchase_order` operations of package `wamn_receiving`.

use wamn_client::{ClientError, FieldDescriptor, RouteMetadata, WamnClient};

/// Every field the `purchase_order` model projects.
pub const PURCHASE_ORDER_FIELDS: &[FieldDescriptor] = &[
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


/// Input for `wamn-receiving:purchase-order/get@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct PurchaseOrderGetRequest {
    /// `uuid`
    pub id: uuid::Uuid,
    /// `string`
    pub request_id: String,
}

/// Result of `wamn-receiving:purchase-order/get@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct PurchaseOrderGetResult {
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

/// Input descriptors for `wamn-receiving:purchase-order/get@1.0.0`.
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

/// Result descriptors for `wamn-receiving:purchase-order/get@1.0.0`.
pub const PURCHASE_ORDER_GET_RESULT: &[FieldDescriptor] = &[
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
/// The grant a caller presents to invoke `wamn-receiving:purchase-order/get@1.0.0`.
pub const PURCHASE_ORDER_GET_GRANT: &str = "wamn-receiving:purchase-order/get@1.0.0";

/// Typed refusals `wamn-receiving:purchase-order/get@1.0.0` declares.
pub const PURCHASE_ORDER_GET_ERRORS: &[&str] = &[
    "internal_error",
    "invalid_input",
    "not_found",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `wamn-receiving:purchase-order/get@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn get_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/purchase_order/get".to_owned(),
    }
}

/// Invoke `wamn-receiving:purchase-order/get@1.0.0` through a bound client.
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


/// Input for `wamn-receiving:purchase-order/query@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct PurchaseOrderQueryRequest {
    /// `text`, omittable
    pub cursor: Option<String>,
    /// `object`, omittable
    pub filter: Option<PurchaseOrderQueryRequestFilter>,
    /// `int64`, omittable
    pub limit: Option<i64>,
    /// `string`
    pub request_id: String,
    /// `object`, omittable
    pub sort: Option<PurchaseOrderQueryRequestSort>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PurchaseOrderQueryRequestFilter {
    /// `array`, omittable
    pub status: Option<Vec<String>>,
    /// `array`, omittable
    pub supplier_id: Option<Vec<uuid::Uuid>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PurchaseOrderQueryRequestSort {
    /// `text`
    pub direction: String,
    /// `text`
    pub field: String,
}

/// Result of `wamn-receiving:purchase-order/query@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct PurchaseOrderQueryResult {
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

/// Input descriptors for `wamn-receiving:purchase-order/query@1.0.0`.
pub const PURCHASE_ORDER_QUERY_INPUT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "cursor",
        type_name: "text",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "filter.status[]",
        type_name: "text",
        nullable: false,
        values: &[
            "cancelled",
            "complete",
            "open",
        ],
    },
    FieldDescriptor {
        path: "filter.supplier_id[]",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "limit",
        type_name: "int64",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "request_id",
        type_name: "string",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "sort.direction",
        type_name: "text",
        nullable: false,
        values: &[
            "ascending",
            "descending",
        ],
    },
    FieldDescriptor {
        path: "sort.field",
        type_name: "text",
        nullable: false,
        values: &[
            "created_at",
            "purchase_order_number",
            "status",
        ],
    },
];

/// Result descriptors for `wamn-receiving:purchase-order/query@1.0.0`.
pub const PURCHASE_ORDER_QUERY_RESULT: &[FieldDescriptor] = &[
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

pub const PURCHASE_ORDER_QUERY_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "cursor", type_name: "text", nullable: false, values: &[] },
required: false, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "filter", type_name: "object", nullable: false, values: &[] },
required: false, minimum: None, maximum: None, children: &[
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "filter.status[]", type_name: "array", nullable: false, values: &[] },
required: false, minimum: None, maximum: None, children: &[
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "filter.status[]", type_name: "text", nullable: false, values: &["cancelled", "complete", "open"] },
required: true, minimum: None, maximum: None, children: &[
], },
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "filter.supplier_id[]", type_name: "array", nullable: false, values: &[] },
required: false, minimum: None, maximum: None, children: &[
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "filter.supplier_id[]", type_name: "uuid", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
], },
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "limit", type_name: "int64", nullable: false, values: &[] },
required: false, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "request_id", type_name: "string", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "sort", type_name: "object", nullable: false, values: &[] },
required: false, minimum: None, maximum: None, children: &[
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "sort.direction", type_name: "text", nullable: false, values: &["ascending", "descending"] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "sort.field", type_name: "text", nullable: false, values: &["created_at", "purchase_order_number", "status"] },
required: true, minimum: None, maximum: None, children: &[
], },
], },
];

pub const PURCHASE_ORDER_QUERY_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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

pub const PURCHASE_ORDER_QUERY_KIND: &str = "query";
pub const PURCHASE_ORDER_QUERY_REQUIRES_COMPOSITION: bool = false;
pub const PURCHASE_ORDER_QUERY_REPLAY: Option<&str> = None;
pub const PURCHASE_ORDER_QUERY_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const PURCHASE_ORDER_QUERY_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `wamn-receiving:purchase-order/query@1.0.0`.
pub const PURCHASE_ORDER_QUERY_GRANT: &str = "wamn-receiving:purchase-order/query@1.0.0";

/// Typed refusals `wamn-receiving:purchase-order/query@1.0.0` declares.
pub const PURCHASE_ORDER_QUERY_ERRORS: &[&str] = &[
    "internal_error",
    "invalid_input",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `wamn-receiving:purchase-order/query@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn query_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/purchase_order/query".to_owned(),
    }
}

/// Invoke `wamn-receiving:purchase-order/query@1.0.0` through a bound client.
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


/// Input for `wamn-receiving:purchase-order/update@1.0.0`.
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
    /// `uuid`, omittable
    pub supplier_id: Option<uuid::Uuid>,
}

/// Result of `wamn-receiving:purchase-order/update@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct PurchaseOrderUpdateResult {
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

/// Input descriptors for `wamn-receiving:purchase-order/update@1.0.0`.
pub const PURCHASE_ORDER_UPDATE_INPUT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "change.supplier_id",
        type_name: "uuid",
        nullable: true,
        values: &[],
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

/// Result descriptors for `wamn-receiving:purchase-order/update@1.0.0`.
pub const PURCHASE_ORDER_UPDATE_RESULT: &[FieldDescriptor] = &[
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
field: FieldDescriptor { path: "change.supplier_id", type_name: "uuid", nullable: false, values: &[] },
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
/// The grant a caller presents to invoke `wamn-receiving:purchase-order/update@1.0.0`.
pub const PURCHASE_ORDER_UPDATE_GRANT: &str = "wamn-receiving:purchase-order/update@1.0.0";

/// Typed refusals `wamn-receiving:purchase-order/update@1.0.0` declares.
pub const PURCHASE_ORDER_UPDATE_ERRORS: &[&str] = &[
    "concurrency_conflict",
    "internal_error",
    "invalid_input",
    "not_found",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `wamn-receiving:purchase-order/update@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn update_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/purchase_order/update".to_owned(),
    }
}

/// Invoke `wamn-receiving:purchase-order/update@1.0.0` through a bound client.
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
