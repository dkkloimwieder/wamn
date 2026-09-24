// @generated from the client-contract IR; do not edit.
//!
//! `product` operations of package `wamn_wms`.

use wamn_client::{ClientError, FieldDescriptor, RouteMetadata, WamnClient};

/// Every field the `product` model projects.
pub const PRODUCT_FIELDS: &[FieldDescriptor] = &[
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
        path: "product_code",
        type_name: "text",
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

/// Input for `wamn-wms:product/create@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProductCreateRequest {
    /// `text`
    pub idempotency_key: String,
    /// `text`
    pub product_code: String,
    /// `string`
    pub request_id: String,
}

/// Result of `wamn-wms:product/create@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProductCreateResult {
    /// `timestamptz`
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `text`
    pub product_code: String,
    /// `int32`
    pub row_version: i32,
}

/// Input descriptors for `wamn-wms:product/create@1.0.0`.
pub const PRODUCT_CREATE_INPUT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "idempotency_key",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "product_code",
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

/// Result descriptors for `wamn-wms:product/create@1.0.0`.
pub const PRODUCT_CREATE_RESULT: &[FieldDescriptor] = &[
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
        path: "product_code",
        type_name: "text",
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

pub const PRODUCT_CREATE_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
            path: "product_code",
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

pub const PRODUCT_CREATE_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
            path: "product_code",
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

pub const PRODUCT_CREATE_KIND: &str = "create";
pub const PRODUCT_CREATE_REQUIRES_COMPOSITION: bool = false;
pub const PRODUCT_CREATE_REPLAY: Option<&str> = Some("claim");
pub const PRODUCT_CREATE_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const PRODUCT_CREATE_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `wamn-wms:product/create@1.0.0`.
pub const PRODUCT_CREATE_GRANT: &str = "wamn-wms:product/create@1.0.0";

/// Typed refusals `wamn-wms:product/create@1.0.0` declares.
pub const PRODUCT_CREATE_ERRORS: &[&str] = &[
    "idempotency_conflict",
    "internal_error",
    "invalid_input",
    "permission_denied",
    "retry",
    "timeout",
    "unique_violation",
];

/// Where the release publishes `wamn-wms:product/create@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn create_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/product/create".to_owned(),
    }
}

/// Invoke `wamn-wms:product/create@1.0.0` through a bound client.
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

/// Input for `wamn-wms:product/get@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProductGetRequest {
    /// `uuid`
    pub id: uuid::Uuid,
    /// `string`
    pub request_id: String,
}

/// Result of `wamn-wms:product/get@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProductGetResult {
    /// `timestamptz`
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `text`
    pub product_code: String,
    /// `int32`
    pub row_version: i32,
}

/// Input descriptors for `wamn-wms:product/get@1.0.0`.
pub const PRODUCT_GET_INPUT: &[FieldDescriptor] = &[
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

/// Result descriptors for `wamn-wms:product/get@1.0.0`.
pub const PRODUCT_GET_RESULT: &[FieldDescriptor] = &[
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
        path: "product_code",
        type_name: "text",
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

pub const PRODUCT_GET_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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

pub const PRODUCT_GET_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
            path: "product_code",
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

pub const PRODUCT_GET_KIND: &str = "get";
pub const PRODUCT_GET_REQUIRES_COMPOSITION: bool = false;
pub const PRODUCT_GET_REPLAY: Option<&str> = None;
pub const PRODUCT_GET_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const PRODUCT_GET_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `wamn-wms:product/get@1.0.0`.
pub const PRODUCT_GET_GRANT: &str = "wamn-wms:product/get@1.0.0";

/// Typed refusals `wamn-wms:product/get@1.0.0` declares.
pub const PRODUCT_GET_ERRORS: &[&str] = &[
    "internal_error",
    "invalid_input",
    "not_found",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `wamn-wms:product/get@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn get_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/product/get".to_owned(),
    }
}

/// Invoke `wamn-wms:product/get@1.0.0` through a bound client.
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

/// Input for `wamn-wms:product/query@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProductQueryRequest {
    /// `text`, omittable
    pub cursor: Option<String>,
    /// `object`, omittable
    pub filter: Option<ProductQueryRequestFilter>,
    /// `int32`, omittable
    pub limit: Option<i32>,
    /// `string`
    pub request_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProductQueryRequestFilter {
    /// `array`, omittable
    pub product_code: Option<Vec<String>>,
}

/// Result of `wamn-wms:product/query@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProductQueryResult {
    /// `timestamptz`
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `text`
    pub product_code: String,
    /// `int32`
    pub row_version: i32,
}

/// Input descriptors for `wamn-wms:product/query@1.0.0`.
pub const PRODUCT_QUERY_INPUT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "cursor",
        type_name: "text",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "filter.product_code[]",
        type_name: "text",
        nullable: false,
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

/// Result descriptors for `wamn-wms:product/query@1.0.0`.
pub const PRODUCT_QUERY_RESULT: &[FieldDescriptor] = &[
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
        path: "product_code",
        type_name: "text",
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

pub const PRODUCT_QUERY_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
            path: "filter",
            type_name: "object",
            nullable: false,
            values: &[],
        },
        required: false,
        minimum: None,
        maximum: None,
        children: &[wamn_client::descriptor::FieldSchema {
            field: FieldDescriptor {
                path: "filter.product_code[]",
                type_name: "array",
                nullable: false,
                values: &[],
            },
            required: false,
            minimum: None,
            maximum: None,
            children: &[wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "filter.product_code[]",
                    type_name: "text",
                    nullable: false,
                    values: &[],
                },
                required: true,
                minimum: None,
                maximum: None,
                children: &[],
            }],
        }],
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

pub const PRODUCT_QUERY_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
            path: "product_code",
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

pub const PRODUCT_QUERY_KIND: &str = "query";
pub const PRODUCT_QUERY_REQUIRES_COMPOSITION: bool = false;
pub const PRODUCT_QUERY_REPLAY: Option<&str> = None;
pub const PRODUCT_QUERY_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const PRODUCT_QUERY_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `wamn-wms:product/query@1.0.0`.
pub const PRODUCT_QUERY_GRANT: &str = "wamn-wms:product/query@1.0.0";

/// Typed refusals `wamn-wms:product/query@1.0.0` declares.
pub const PRODUCT_QUERY_ERRORS: &[&str] = &[
    "internal_error",
    "invalid_input",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `wamn-wms:product/query@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn query_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/product/query".to_owned(),
    }
}

/// Invoke `wamn-wms:product/query@1.0.0` through a bound client.
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

/// Input for `wamn-wms:product/update@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProductUpdateRequest {
    /// `object`
    pub change: ProductUpdateRequestChange,
    /// `int32`
    pub expected_row_version: i32,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `string`
    pub request_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProductUpdateRequestChange {
    /// `text`, omittable
    pub product_code: Option<String>,
}

/// Result of `wamn-wms:product/update@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProductUpdateResult {
    /// `timestamptz`
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `text`
    pub product_code: String,
    /// `int32`
    pub row_version: i32,
}

/// Input descriptors for `wamn-wms:product/update@1.0.0`.
pub const PRODUCT_UPDATE_INPUT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "change.product_code",
        type_name: "text",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "expected_row_version",
        type_name: "int32",
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

/// Result descriptors for `wamn-wms:product/update@1.0.0`.
pub const PRODUCT_UPDATE_RESULT: &[FieldDescriptor] = &[
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
        path: "product_code",
        type_name: "text",
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

pub const PRODUCT_UPDATE_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "change",
            type_name: "object",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[wamn_client::descriptor::FieldSchema {
            field: FieldDescriptor {
                path: "change.product_code",
                type_name: "text",
                nullable: false,
                values: &[],
            },
            required: false,
            minimum: None,
            maximum: None,
            children: &[],
        }],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "expected_row_version",
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

pub const PRODUCT_UPDATE_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
            path: "product_code",
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

pub const PRODUCT_UPDATE_KIND: &str = "update";
pub const PRODUCT_UPDATE_REQUIRES_COMPOSITION: bool = false;
pub const PRODUCT_UPDATE_REPLAY: Option<&str> = None;
pub const PRODUCT_UPDATE_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const PRODUCT_UPDATE_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `wamn-wms:product/update@1.0.0`.
pub const PRODUCT_UPDATE_GRANT: &str = "wamn-wms:product/update@1.0.0";

/// Typed refusals `wamn-wms:product/update@1.0.0` declares.
pub const PRODUCT_UPDATE_ERRORS: &[&str] = &[
    "concurrency_conflict",
    "internal_error",
    "invalid_input",
    "not_found",
    "permission_denied",
    "retry",
    "timeout",
    "unique_violation",
];

/// Where the release publishes `wamn-wms:product/update@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn update_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/product/update".to_owned(),
    }
}

/// Invoke `wamn-wms:product/update@1.0.0` through a bound client.
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
