// @generated from the client-contract IR; do not edit.
//!
//! `packaging` operations of package `wamn_wms`.

use wamn_client::{ClientError, FieldDescriptor, RouteMetadata, WamnClient};

/// Every field the `packaging` model projects.
pub const PACKAGING_FIELDS: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "code",
        type_name: "text",
        nullable: false,
        values: &[],
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
        path: "row_version",
        type_name: "int32",
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

/// Input for `wamn-wms:packaging/close@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct PackagingCloseRequest {
    /// `text`
    pub request_id: String,
    /// `object`
    pub value: PackagingCloseRequestValue,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PackagingCloseRequestValue {
    /// `int32`
    pub expected_row_version: i32,
    /// `text`
    pub idempotency_key: String,
    /// `uuid`
    pub packaging_id: uuid::Uuid,
}

/// Result of `wamn-wms:packaging/close@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct PackagingCloseResult {
    /// `text`
    pub code: String,
    /// `text`
    pub lifecycle: String,
    /// `uuid`
    pub location_id: uuid::Uuid,
    /// `uuid`
    pub operation_id: uuid::Uuid,
    /// `uuid`
    pub packaging_id: uuid::Uuid,
    /// `int32`
    pub row_version: i32,
    /// `text`
    pub r#type: String,
}

/// Input descriptors for `wamn-wms:packaging/close@1.0.0`.
pub const PACKAGING_CLOSE_INPUT: &[FieldDescriptor] = &[
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
        path: "value.packaging_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
];

/// Result descriptors for `wamn-wms:packaging/close@1.0.0`.
pub const PACKAGING_CLOSE_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "code",
        type_name: "text",
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
        path: "row_version",
        type_name: "int32",
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

pub const PACKAGING_CLOSE_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
                    path: "value.packaging_id",
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

pub const PACKAGING_CLOSE_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "code",
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

pub const PACKAGING_CLOSE_KIND: &str = "command";
pub const PACKAGING_CLOSE_REQUIRES_COMPOSITION: bool = true;
pub const PACKAGING_CLOSE_REPLAY: Option<&str> = Some("claim");
pub const PACKAGING_CLOSE_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const PACKAGING_CLOSE_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `wamn-wms:packaging/close@1.0.0`.
pub const PACKAGING_CLOSE_GRANT: &str = "wamn-wms:packaging/close@1.0.0";

/// Typed refusals `wamn-wms:packaging/close@1.0.0` declares.
pub const PACKAGING_CLOSE_ERRORS: &[&str] = &[
    "concurrency_conflict",
    "idempotency_conflict",
    "internal_error",
    "invalid_input",
    "not_found",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `wamn-wms:packaging/close@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn close_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/packaging/close".to_owned(),
    }
}

/// Invoke `wamn-wms:packaging/close@1.0.0` through a bound client.
///
/// # Errors
///
/// [`ClientError`] for a transport failure, a refusal, or a response that
/// does not match the operation's envelope.
pub async fn close(
    client: &WamnClient,
    items: &[serde_json::Value],
) -> Result<Vec<wamn_client::ItemOutcome>, ClientError> {
    client
        .invoke(&close_route(), &std::collections::BTreeMap::new(), items)
        .await
}

/// Input for `wamn-wms:packaging/create@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct PackagingCreateRequest {
    /// `text`
    pub request_id: String,
    /// `object`
    pub value: PackagingCreateRequestValue,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PackagingCreateRequestValue {
    /// `text`
    pub code: String,
    /// `text`
    pub idempotency_key: String,
    /// `uuid`
    pub location_id: uuid::Uuid,
    /// `text`
    pub r#type: String,
}

/// Result of `wamn-wms:packaging/create@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct PackagingCreateResult {
    /// `text`
    pub code: String,
    /// `text`
    pub lifecycle: String,
    /// `uuid`
    pub location_id: uuid::Uuid,
    /// `uuid`
    pub operation_id: uuid::Uuid,
    /// `uuid`
    pub packaging_id: uuid::Uuid,
    /// `int32`
    pub row_version: i32,
    /// `text`
    pub r#type: String,
}

/// Input descriptors for `wamn-wms:packaging/create@1.0.0`.
pub const PACKAGING_CREATE_INPUT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "request_id",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.code",
        type_name: "text",
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
        path: "value.location_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.type",
        type_name: "text",
        nullable: false,
        values: &[],
    },
];

/// Result descriptors for `wamn-wms:packaging/create@1.0.0`.
pub const PACKAGING_CREATE_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "code",
        type_name: "text",
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
        path: "row_version",
        type_name: "int32",
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

pub const PACKAGING_CREATE_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
                    path: "value.code",
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
                    path: "value.location_id",
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
                    path: "value.type",
                    type_name: "text",
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

pub const PACKAGING_CREATE_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "code",
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

pub const PACKAGING_CREATE_KIND: &str = "command";
pub const PACKAGING_CREATE_REQUIRES_COMPOSITION: bool = false;
pub const PACKAGING_CREATE_REPLAY: Option<&str> = Some("claim");
pub const PACKAGING_CREATE_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const PACKAGING_CREATE_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `wamn-wms:packaging/create@1.0.0`.
pub const PACKAGING_CREATE_GRANT: &str = "wamn-wms:packaging/create@1.0.0";

/// Typed refusals `wamn-wms:packaging/create@1.0.0` declares.
pub const PACKAGING_CREATE_ERRORS: &[&str] = &[
    "concurrency_conflict",
    "idempotency_conflict",
    "internal_error",
    "invalid_input",
    "not_found",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `wamn-wms:packaging/create@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn create_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/packaging/create".to_owned(),
    }
}

/// Invoke `wamn-wms:packaging/create@1.0.0` through a bound client.
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

/// Input for `wamn-wms:packaging/get@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct PackagingGetRequest {
    /// `uuid`
    pub id: uuid::Uuid,
}

/// Result of `wamn-wms:packaging/get@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct PackagingGetResult {
    /// `text`
    pub code: String,
    /// `timestamptz`
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `text`
    pub lifecycle: String,
    /// `uuid`
    pub location_id: uuid::Uuid,
    /// `int32`
    pub row_version: i32,
    /// `text`
    pub r#type: String,
}

/// Input descriptors for `wamn-wms:packaging/get@1.0.0`.
pub const PACKAGING_GET_INPUT: &[FieldDescriptor] = &[FieldDescriptor {
    path: "id",
    type_name: "uuid",
    nullable: false,
    values: &[],
}];

/// Result descriptors for `wamn-wms:packaging/get@1.0.0`.
pub const PACKAGING_GET_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "code",
        type_name: "text",
        nullable: false,
        values: &[],
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
        path: "row_version",
        type_name: "int32",
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

pub const PACKAGING_GET_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] =
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

pub const PACKAGING_GET_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "code",
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

pub const PACKAGING_GET_KIND: &str = "get";
pub const PACKAGING_GET_REQUIRES_COMPOSITION: bool = false;
pub const PACKAGING_GET_REPLAY: Option<&str> = None;
pub const PACKAGING_GET_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const PACKAGING_GET_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `wamn-wms:packaging/get@1.0.0`.
pub const PACKAGING_GET_GRANT: &str = "wamn-wms:packaging/get@1.0.0";

/// Typed refusals `wamn-wms:packaging/get@1.0.0` declares.
pub const PACKAGING_GET_ERRORS: &[&str] = &[
    "internal_error",
    "invalid_input",
    "not_found",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `wamn-wms:packaging/get@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn get_route() -> RouteMetadata {
    RouteMetadata {
        method: "GET".to_owned(),
        template: "/packaging/get".to_owned(),
    }
}

/// Invoke `wamn-wms:packaging/get@1.0.0` through a bound client.
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

/// Input for `wamn-wms:packaging/query@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct PackagingQueryRequest {
    /// `text`, omittable
    pub cursor: Option<String>,
    /// `int32`, omittable
    pub limit: Option<i32>,
}

/// Result of `wamn-wms:packaging/query@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct PackagingQueryResult {
    /// `text`
    pub code: String,
    /// `timestamptz`
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `text`
    pub lifecycle: String,
    /// `uuid`
    pub location_id: uuid::Uuid,
    /// `int32`
    pub row_version: i32,
    /// `text`
    pub r#type: String,
}

/// Input descriptors for `wamn-wms:packaging/query@1.0.0`.
pub const PACKAGING_QUERY_INPUT: &[FieldDescriptor] = &[
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

/// Result descriptors for `wamn-wms:packaging/query@1.0.0`.
pub const PACKAGING_QUERY_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "code",
        type_name: "text",
        nullable: false,
        values: &[],
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
        path: "row_version",
        type_name: "int32",
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

pub const PACKAGING_QUERY_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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

pub const PACKAGING_QUERY_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "code",
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

pub const PACKAGING_QUERY_KIND: &str = "query";
pub const PACKAGING_QUERY_REQUIRES_COMPOSITION: bool = false;
pub const PACKAGING_QUERY_REPLAY: Option<&str> = None;
pub const PACKAGING_QUERY_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const PACKAGING_QUERY_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `wamn-wms:packaging/query@1.0.0`.
pub const PACKAGING_QUERY_GRANT: &str = "wamn-wms:packaging/query@1.0.0";

/// Typed refusals `wamn-wms:packaging/query@1.0.0` declares.
pub const PACKAGING_QUERY_ERRORS: &[&str] = &[
    "internal_error",
    "invalid_input",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `wamn-wms:packaging/query@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn query_route() -> RouteMetadata {
    RouteMetadata {
        method: "GET".to_owned(),
        template: "/packaging/query".to_owned(),
    }
}

/// Invoke `wamn-wms:packaging/query@1.0.0` through a bound client.
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
