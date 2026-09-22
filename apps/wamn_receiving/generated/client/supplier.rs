// @generated from the client-contract IR; do not edit.
//!
//! `supplier` operations of package `wamn_receiving`.

use wamn_client::{ClientError, FieldDescriptor, RouteMetadata, WamnClient};

/// Every field the `supplier` model projects.
pub const SUPPLIER_FIELDS: &[FieldDescriptor] = &[
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
        path: "name",
        type_name: "text",
        nullable: false,
        values: &[],
    },
];

/// Input for `wamn-receiving:supplier/create@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct SupplierCreateRequest {
    /// `text`
    pub idempotency_key: String,
    /// `text`
    pub name: String,
    /// `string`
    pub request_id: String,
}

/// Result of `wamn-receiving:supplier/create@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct SupplierCreateResult {
    /// `timestamptz`
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `text`
    pub name: String,
}

/// Input descriptors for `wamn-receiving:supplier/create@1.0.0`.
pub const SUPPLIER_CREATE_INPUT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "idempotency_key",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "name",
        type_name: "text",
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

/// Result descriptors for `wamn-receiving:supplier/create@1.0.0`.
pub const SUPPLIER_CREATE_RESULT: &[FieldDescriptor] = &[
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
        path: "name",
        type_name: "text",
        nullable: false,
        values: &[],
    },
];

pub const SUPPLIER_CREATE_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
            path: "name",
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

pub const SUPPLIER_CREATE_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
            path: "name",
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

pub const SUPPLIER_CREATE_KIND: &str = "create";
pub const SUPPLIER_CREATE_REQUIRES_COMPOSITION: bool = false;
pub const SUPPLIER_CREATE_REPLAY: Option<&str> = Some("claim");
pub const SUPPLIER_CREATE_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const SUPPLIER_CREATE_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `wamn-receiving:supplier/create@1.0.0`.
pub const SUPPLIER_CREATE_GRANT: &str = "wamn-receiving:supplier/create@1.0.0";

/// Typed refusals `wamn-receiving:supplier/create@1.0.0` declares.
pub const SUPPLIER_CREATE_ERRORS: &[&str] = &[
    "idempotency_conflict",
    "internal_error",
    "invalid_input",
    "permission_denied",
    "retry",
    "timeout",
    "unique_violation",
];

/// Where the release publishes `wamn-receiving:supplier/create@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn create_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/supplier/create".to_owned(),
    }
}

/// Invoke `wamn-receiving:supplier/create@1.0.0` through a bound client.
///
/// # Errors
///
/// [`ClientError`] for a transport failure, a refusal, or a response that
/// does not match the operation's envelope.
pub async fn create(
    client: &WamnClient,
    items: &[serde_json::Value],
) -> Result<Vec<wamn_client::ItemOutcome>, ClientError> {
    client
        .invoke(&create_route(), &std::collections::BTreeMap::new(), items)
        .await
}

/// Input for `wamn-receiving:supplier/query@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct SupplierQueryRequest {
    /// `text`, omittable
    pub cursor: Option<String>,
    /// `int32`, omittable
    pub limit: Option<i32>,
    /// `string`
    pub request_id: String,
}

/// Result of `wamn-receiving:supplier/query@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct SupplierQueryResult {
    /// `timestamptz`
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `text`
    pub name: String,
}

/// Input descriptors for `wamn-receiving:supplier/query@1.0.0`.
pub const SUPPLIER_QUERY_INPUT: &[FieldDescriptor] = &[
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

/// Result descriptors for `wamn-receiving:supplier/query@1.0.0`.
pub const SUPPLIER_QUERY_RESULT: &[FieldDescriptor] = &[
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
        path: "name",
        type_name: "text",
        nullable: false,
        values: &[],
    },
];

pub const SUPPLIER_QUERY_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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

pub const SUPPLIER_QUERY_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
            path: "name",
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

pub const SUPPLIER_QUERY_KIND: &str = "query";
pub const SUPPLIER_QUERY_REQUIRES_COMPOSITION: bool = false;
pub const SUPPLIER_QUERY_REPLAY: Option<&str> = None;
pub const SUPPLIER_QUERY_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const SUPPLIER_QUERY_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `wamn-receiving:supplier/query@1.0.0`.
pub const SUPPLIER_QUERY_GRANT: &str = "wamn-receiving:supplier/query@1.0.0";

/// Typed refusals `wamn-receiving:supplier/query@1.0.0` declares.
pub const SUPPLIER_QUERY_ERRORS: &[&str] = &[
    "internal_error",
    "invalid_input",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `wamn-receiving:supplier/query@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn query_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/supplier/query".to_owned(),
    }
}

/// Invoke `wamn-receiving:supplier/query@1.0.0` through a bound client.
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
