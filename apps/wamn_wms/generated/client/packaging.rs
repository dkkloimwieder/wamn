// @generated from the client-contract IR; do not edit.
//!
//! `packaging` operations of package `wamn_wms`.

use wamn_client::{ClientError, FieldDescriptor, RouteMetadata, WamnClient};

/// Every field the `packaging` model projects.
pub const PACKAGING_FIELDS: &[FieldDescriptor] = &[
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
        path: "id",
        type_name: "uuid",
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
        path: "packaging_code",
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
    FieldDescriptor {
        path: "status",
        type_name: "text",
        nullable: false,
        values: &["available", "consumed", "held"],
    },
    FieldDescriptor {
        path: "type",
        type_name: "text",
        nullable: false,
        values: &["bin", "case", "loose", "pallet", "tote"],
    },
    FieldDescriptor {
        path: "updated_at",
        type_name: "timestamptz",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "updated_by",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
];

/// Input for `wamn-wms:packaging/create@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct PackagingCreateRequest {
    /// `text`
    pub idempotency_key: String,
    /// `uuid`
    pub location_id: uuid::Uuid,
    /// `text`
    pub packaging_code: String,
    /// `string`
    pub request_id: String,
    /// `text`
    pub status: String,
    /// `text`
    pub r#type: String,
}

/// Result of `wamn-wms:packaging/create@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct PackagingCreateResult {
    /// `timestamptz`
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// `uuid`
    pub created_by: uuid::Uuid,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `uuid`
    pub location_id: uuid::Uuid,
    /// `text`
    pub packaging_code: String,
    /// `int32`
    pub row_version: i32,
    /// `text`
    pub status: String,
    /// `text`
    pub r#type: String,
    /// `timestamptz`
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// `uuid`
    pub updated_by: uuid::Uuid,
}

/// Input descriptors for `wamn-wms:packaging/create@1.0.0`.
pub const PACKAGING_CREATE_INPUT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "idempotency_key",
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
        path: "packaging_code",
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
    FieldDescriptor {
        path: "status",
        type_name: "text",
        nullable: false,
        values: &["available", "held"],
    },
    FieldDescriptor {
        path: "type",
        type_name: "text",
        nullable: false,
        values: &["bin", "case", "loose", "pallet", "tote"],
    },
];

/// Result descriptors for `wamn-wms:packaging/create@1.0.0`.
pub const PACKAGING_CREATE_RESULT: &[FieldDescriptor] = &[
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
        path: "id",
        type_name: "uuid",
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
        path: "packaging_code",
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
    FieldDescriptor {
        path: "status",
        type_name: "text",
        nullable: false,
        values: &["available", "consumed", "held"],
    },
    FieldDescriptor {
        path: "type",
        type_name: "text",
        nullable: false,
        values: &["bin", "case", "loose", "pallet", "tote"],
    },
    FieldDescriptor {
        path: "updated_at",
        type_name: "timestamptz",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "updated_by",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
];

pub const PACKAGING_CREATE_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
            path: "packaging_code",
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
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "status",
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
            path: "type",
            type_name: "text",
            nullable: false,
            values: &["bin", "case", "loose", "pallet", "tote"],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
];

pub const PACKAGING_CREATE_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
            path: "packaging_code",
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
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "status",
            type_name: "text",
            nullable: false,
            values: &["available", "consumed", "held"],
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
            values: &["bin", "case", "loose", "pallet", "tote"],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "updated_at",
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
            path: "updated_by",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
];

pub const PACKAGING_CREATE_KIND: &str = "create";
pub const PACKAGING_CREATE_REQUIRES_COMPOSITION: bool = false;
pub const PACKAGING_CREATE_REPLAY: Option<&str> = Some("claim");
pub const PACKAGING_CREATE_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const PACKAGING_CREATE_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `wamn-wms:packaging/create@1.0.0`.
pub const PACKAGING_CREATE_GRANT: &str = "wamn-wms:packaging/create@1.0.0";

/// Typed refusals `wamn-wms:packaging/create@1.0.0` declares.
pub const PACKAGING_CREATE_ERRORS: &[&str] = &[
    "check_violation",
    "foreign_key_violation",
    "idempotency_conflict",
    "internal_error",
    "invalid_input",
    "permission_denied",
    "retry",
    "timeout",
    "unique_violation",
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
    /// `timestamptz`
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// `uuid`
    pub created_by: uuid::Uuid,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `uuid`
    pub location_id: uuid::Uuid,
    /// `text`
    pub packaging_code: String,
    /// `int32`
    pub row_version: i32,
    /// `text`
    pub status: String,
    /// `text`
    pub r#type: String,
    /// `timestamptz`
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// `uuid`
    pub updated_by: uuid::Uuid,
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
        path: "id",
        type_name: "uuid",
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
        path: "packaging_code",
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
    FieldDescriptor {
        path: "status",
        type_name: "text",
        nullable: false,
        values: &["available", "consumed", "held"],
    },
    FieldDescriptor {
        path: "type",
        type_name: "text",
        nullable: false,
        values: &["bin", "case", "loose", "pallet", "tote"],
    },
    FieldDescriptor {
        path: "updated_at",
        type_name: "timestamptz",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "updated_by",
        type_name: "uuid",
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
            path: "packaging_code",
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
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "status",
            type_name: "text",
            nullable: false,
            values: &["available", "consumed", "held"],
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
            values: &["bin", "case", "loose", "pallet", "tote"],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "updated_at",
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
            path: "updated_by",
            type_name: "uuid",
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
    /// `object`, omittable
    pub filter: Option<PackagingQueryRequestFilter>,
    /// `int32`, omittable
    pub limit: Option<i32>,
    /// `object`, omittable
    pub sort: Option<PackagingQueryRequestSort>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PackagingQueryRequestFilter {
    /// `array`, omittable
    pub location_id: Option<Vec<uuid::Uuid>>,
    /// `array`, omittable
    pub packaging_code: Option<Vec<String>>,
    /// `array`, omittable
    pub status: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PackagingQueryRequestSort {
    /// `text`
    pub direction: String,
    /// `text`
    pub field: String,
}

/// Result of `wamn-wms:packaging/query@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct PackagingQueryResult {
    /// `timestamptz`
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// `uuid`
    pub created_by: uuid::Uuid,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `uuid`
    pub location_id: uuid::Uuid,
    /// `text`
    pub packaging_code: String,
    /// `int32`
    pub row_version: i32,
    /// `text`
    pub status: String,
    /// `text`
    pub r#type: String,
    /// `timestamptz`
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// `uuid`
    pub updated_by: uuid::Uuid,
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
        path: "filter.location_id[]",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "filter.packaging_code[]",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "filter.status[]",
        type_name: "text",
        nullable: false,
        values: &["available", "consumed", "held"],
    },
    FieldDescriptor {
        path: "limit",
        type_name: "int32",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "sort.direction",
        type_name: "text",
        nullable: false,
        values: &["ascending", "descending"],
    },
    FieldDescriptor {
        path: "sort.field",
        type_name: "text",
        nullable: false,
        values: &["created_at", "location_id", "packaging_code", "updated_at"],
    },
];

/// Result descriptors for `wamn-wms:packaging/query@1.0.0`.
pub const PACKAGING_QUERY_RESULT: &[FieldDescriptor] = &[
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
        path: "id",
        type_name: "uuid",
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
        path: "packaging_code",
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
    FieldDescriptor {
        path: "status",
        type_name: "text",
        nullable: false,
        values: &["available", "consumed", "held"],
    },
    FieldDescriptor {
        path: "type",
        type_name: "text",
        nullable: false,
        values: &["bin", "case", "loose", "pallet", "tote"],
    },
    FieldDescriptor {
        path: "updated_at",
        type_name: "timestamptz",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "updated_by",
        type_name: "uuid",
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
            path: "filter",
            type_name: "object",
            nullable: false,
            values: &[],
        },
        required: false,
        minimum: None,
        maximum: None,
        children: &[
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "filter.location_id[]",
                    type_name: "array",
                    nullable: false,
                    values: &[],
                },
                required: false,
                minimum: None,
                maximum: None,
                children: &[wamn_client::descriptor::FieldSchema {
                    field: FieldDescriptor {
                        path: "filter.location_id[]",
                        type_name: "uuid",
                        nullable: false,
                        values: &[],
                    },
                    required: true,
                    minimum: None,
                    maximum: None,
                    children: &[],
                }],
            },
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "filter.packaging_code[]",
                    type_name: "array",
                    nullable: false,
                    values: &[],
                },
                required: false,
                minimum: None,
                maximum: None,
                children: &[wamn_client::descriptor::FieldSchema {
                    field: FieldDescriptor {
                        path: "filter.packaging_code[]",
                        type_name: "text",
                        nullable: false,
                        values: &[],
                    },
                    required: true,
                    minimum: None,
                    maximum: None,
                    children: &[],
                }],
            },
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "filter.status[]",
                    type_name: "array",
                    nullable: false,
                    values: &[],
                },
                required: false,
                minimum: None,
                maximum: None,
                children: &[wamn_client::descriptor::FieldSchema {
                    field: FieldDescriptor {
                        path: "filter.status[]",
                        type_name: "text",
                        nullable: false,
                        values: &["available", "consumed", "held"],
                    },
                    required: true,
                    minimum: None,
                    maximum: None,
                    children: &[],
                }],
            },
        ],
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
            path: "sort",
            type_name: "object",
            nullable: false,
            values: &[],
        },
        required: false,
        minimum: None,
        maximum: None,
        children: &[
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "sort.direction",
                    type_name: "text",
                    nullable: false,
                    values: &["ascending", "descending"],
                },
                required: true,
                minimum: None,
                maximum: None,
                children: &[],
            },
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "sort.field",
                    type_name: "text",
                    nullable: false,
                    values: &["created_at", "location_id", "packaging_code", "updated_at"],
                },
                required: true,
                minimum: None,
                maximum: None,
                children: &[],
            },
        ],
    },
];

pub const PACKAGING_QUERY_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
            path: "packaging_code",
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
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "status",
            type_name: "text",
            nullable: false,
            values: &["available", "consumed", "held"],
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
            values: &["bin", "case", "loose", "pallet", "tote"],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "updated_at",
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
            path: "updated_by",
            type_name: "uuid",
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
