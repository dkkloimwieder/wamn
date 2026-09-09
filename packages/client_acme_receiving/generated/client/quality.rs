// @generated from the client-contract IR; do not edit.
//!
//! `quality` operations of package `client_acme_receiving`.

use wamn_client::{ClientError, FieldDescriptor, RouteMetadata, WamnClient};

/// Every field the `quality` model projects.
pub const QUALITY_FIELDS: &[FieldDescriptor] = &[
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
        path: "id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "purchase_order_id",
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
        path: "purchase_order_row_version",
        type_name: "int64",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "receipt_id",
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
        path: "status",
        type_name: "text",
        nullable: false,
        values: &[
            "approved",
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
];


/// Input for `client-acme-receiving:quality/approve-inspection@3.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct QualityApproveInspectionRequest {
    /// `int64`
    pub expected_row_version: i64,
    /// `uuid`
    pub receipt_id: uuid::Uuid,
    /// `text`
    pub request_id: String,
}

/// Result of `client-acme-receiving:quality/approve-inspection@3.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct QualityApproveInspectionResult {
    /// `uuid`
    pub purchase_order_id: uuid::Uuid,
    /// `int64`
    pub purchase_order_row_version: i64,
    /// `uuid`
    pub receipt_id: uuid::Uuid,
    /// `int64`
    pub row_version: i64,
    /// `text`
    pub status: String,
}

/// Input descriptors for `client-acme-receiving:quality/approve-inspection@3.0.0`.
pub const QUALITY_APPROVE_INSPECTION_INPUT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "expected_row_version",
        type_name: "int64",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "receipt_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "request_id",
        type_name: "text",
        nullable: false,
        values: &[],
    },
];

/// Result descriptors for `client-acme-receiving:quality/approve-inspection@3.0.0`.
pub const QUALITY_APPROVE_INSPECTION_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "purchase_order_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "purchase_order_row_version",
        type_name: "int64",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "receipt_id",
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
        path: "status",
        type_name: "text",
        nullable: false,
        values: &[
            "approved",
        ],
    },
];

pub const QUALITY_APPROVE_INSPECTION_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "expected_row_version", type_name: "int64", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "receipt_id", type_name: "uuid", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "request_id", type_name: "text", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
];

pub const QUALITY_APPROVE_INSPECTION_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "purchase_order_id", type_name: "uuid", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "purchase_order_row_version", type_name: "int64", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "receipt_id", type_name: "uuid", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "row_version", type_name: "int64", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "status", type_name: "text", nullable: false, values: &["approved"] },
required: true, minimum: None, maximum: None, children: &[
], },
];

pub const QUALITY_APPROVE_INSPECTION_KIND: &str = "command";
pub const QUALITY_APPROVE_INSPECTION_REQUIRES_COMPOSITION: bool = true;
pub const QUALITY_APPROVE_INSPECTION_REPLAY: Option<&str> = Some("state");
pub const QUALITY_APPROVE_INSPECTION_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const QUALITY_APPROVE_INSPECTION_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `client-acme-receiving:quality/approve-inspection@3.0.0`.
pub const QUALITY_APPROVE_INSPECTION_GRANT: &str = "client-acme-receiving:quality/approve-inspection@3.0.0";

/// Typed refusals `client-acme-receiving:quality/approve-inspection@3.0.0` declares.
pub const QUALITY_APPROVE_INSPECTION_ERRORS: &[&str] = &[
    "concurrency_conflict",
    "internal_error",
    "invalid_input",
    "not_found",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `client-acme-receiving:quality/approve-inspection@3.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn approve_inspection_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/acme/quality/approve_inspection".to_owned(),
    }
}

/// Invoke `client-acme-receiving:quality/approve-inspection@3.0.0` through a bound client.
///
/// # Errors
///
/// [`ClientError`] for a transport failure, a refusal, or a response that
/// does not match the operation's envelope.
pub async fn approve_inspection(
    client: &WamnClient,
    items: &[serde_json::Value],
) -> Result<Vec<wamn_client::ItemOutcome>, ClientError> {
    client
        .invoke(&approve_inspection_route(), &std::collections::BTreeMap::new(), items)
        .await
}


/// Input for `client-acme-receiving:quality/load-purchase-order-detail@3.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct QualityLoadPurchaseOrderDetailRequest {
    /// `uuid`
    pub purchase_order_id: uuid::Uuid,
    /// `text`
    pub request_id: String,
}

/// Result of `client-acme-receiving:quality/load-purchase-order-detail@3.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct QualityLoadPurchaseOrderDetailResult {
    /// `boolean`
    pub acme_inspection_required: bool,
    /// `text`
    pub acme_quality_status: String,
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
}

/// Input descriptors for `client-acme-receiving:quality/load-purchase-order-detail@3.0.0`.
pub const QUALITY_LOAD_PURCHASE_ORDER_DETAIL_INPUT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "purchase_order_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "request_id",
        type_name: "text",
        nullable: false,
        values: &[],
    },
];

/// Result descriptors for `client-acme-receiving:quality/load-purchase-order-detail@3.0.0`.
pub const QUALITY_LOAD_PURCHASE_ORDER_DETAIL_RESULT: &[FieldDescriptor] = &[
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
];

pub const QUALITY_LOAD_PURCHASE_ORDER_DETAIL_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "purchase_order_id", type_name: "uuid", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "request_id", type_name: "text", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
];

pub const QUALITY_LOAD_PURCHASE_ORDER_DETAIL_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "acme_inspection_required", type_name: "boolean", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "acme_quality_status", type_name: "text", nullable: false, values: &["approved", "not_required", "pending"] },
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
];

pub const QUALITY_LOAD_PURCHASE_ORDER_DETAIL_KIND: &str = "projection";
pub const QUALITY_LOAD_PURCHASE_ORDER_DETAIL_REQUIRES_COMPOSITION: bool = false;
pub const QUALITY_LOAD_PURCHASE_ORDER_DETAIL_REPLAY: Option<&str> = None;
pub const QUALITY_LOAD_PURCHASE_ORDER_DETAIL_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const QUALITY_LOAD_PURCHASE_ORDER_DETAIL_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `client-acme-receiving:quality/load-purchase-order-detail@3.0.0`.
pub const QUALITY_LOAD_PURCHASE_ORDER_DETAIL_GRANT: &str = "client-acme-receiving:quality/load-purchase-order-detail@3.0.0";

/// Typed refusals `client-acme-receiving:quality/load-purchase-order-detail@3.0.0` declares.
pub const QUALITY_LOAD_PURCHASE_ORDER_DETAIL_ERRORS: &[&str] = &[
    "internal_error",
    "invalid_input",
    "not_found",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `client-acme-receiving:quality/load-purchase-order-detail@3.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn load_purchase_order_detail_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/acme/quality/load_purchase_order_detail".to_owned(),
    }
}

/// Invoke `client-acme-receiving:quality/load-purchase-order-detail@3.0.0` through a bound client.
///
/// # Errors
///
/// [`ClientError`] for a transport failure, a refusal, or a response that
/// does not match the operation's envelope.
pub async fn load_purchase_order_detail(
    client: &WamnClient,
    items: &[serde_json::Value],
) -> Result<Vec<wamn_client::ItemOutcome>, ClientError> {
    client
        .invoke(&load_purchase_order_detail_route(), &std::collections::BTreeMap::new(), items)
        .await
}
