// @generated from the client-contract IR; do not edit.
//!
//! `pallet` operations of package `wamn_wms`.

use wamn_client::{ClientError, FieldDescriptor, RouteMetadata, WamnClient};

/// Every field the `pallet` model projects.
pub const PALLET_FIELDS: &[FieldDescriptor] = &[
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
        path: "location_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "pallet_code",
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
            "available",
            "consumed",
            "held",
        ],
    },
    FieldDescriptor {
        path: "updated_at",
        type_name: "timestamptz",
        nullable: false,
        values: &[],
    },
];


/// Input for `wamn-wms:pallet/get@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct PalletGetRequest {
    /// `uuid`
    pub id: uuid::Uuid,
    /// `string`
    pub request_id: String,
}

/// Result of `wamn-wms:pallet/get@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct PalletGetResult {
    /// `timestamptz`
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `uuid`
    pub location_id: uuid::Uuid,
    /// `text`
    pub pallet_code: String,
    /// `int64`
    pub row_version: i64,
    /// `text`
    pub status: String,
    /// `timestamptz`
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Input descriptors for `wamn-wms:pallet/get@1.0.0`.
pub const PALLET_GET_INPUT: &[FieldDescriptor] = &[
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

/// Result descriptors for `wamn-wms:pallet/get@1.0.0`.
pub const PALLET_GET_RESULT: &[FieldDescriptor] = &[
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
        path: "location_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "pallet_code",
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
            "available",
            "consumed",
            "held",
        ],
    },
    FieldDescriptor {
        path: "updated_at",
        type_name: "timestamptz",
        nullable: false,
        values: &[],
    },
];

pub const PALLET_GET_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "id", type_name: "uuid", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "request_id", type_name: "string", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
];

pub const PALLET_GET_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "created_at", type_name: "timestamptz", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "id", type_name: "uuid", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "location_id", type_name: "uuid", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "pallet_code", type_name: "text", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "row_version", type_name: "int64", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "status", type_name: "text", nullable: false, values: &["available", "consumed", "held"] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "updated_at", type_name: "timestamptz", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
];

pub const PALLET_GET_KIND: &str = "get";
pub const PALLET_GET_REQUIRES_COMPOSITION: bool = false;
pub const PALLET_GET_REPLAY: Option<&str> = None;
pub const PALLET_GET_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const PALLET_GET_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `wamn-wms:pallet/get@1.0.0`.
pub const PALLET_GET_GRANT: &str = "wamn-wms:pallet/get@1.0.0";

/// Typed refusals `wamn-wms:pallet/get@1.0.0` declares.
pub const PALLET_GET_ERRORS: &[&str] = &[
    "internal_error",
    "invalid_input",
    "not_found",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `wamn-wms:pallet/get@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn get_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/pallet/get".to_owned(),
    }
}

/// Invoke `wamn-wms:pallet/get@1.0.0` through a bound client.
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


/// Input for `wamn-wms:pallet/query@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct PalletQueryRequest {
    /// `text`, omittable
    pub cursor: Option<String>,
    /// `object`, omittable
    pub filter: Option<PalletQueryRequestFilter>,
    /// `int64`, omittable
    pub limit: Option<i64>,
    /// `string`
    pub request_id: String,
    /// `object`, omittable
    pub sort: Option<PalletQueryRequestSort>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PalletQueryRequestFilter {
    /// `array`, omittable
    pub location_id: Option<Vec<uuid::Uuid>>,
    /// `array`, omittable
    pub pallet_code: Option<Vec<String>>,
    /// `array`, omittable
    pub status: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PalletQueryRequestSort {
    /// `text`
    pub direction: String,
    /// `text`
    pub field: String,
}

/// Result of `wamn-wms:pallet/query@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct PalletQueryResult {
    /// `timestamptz`
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `uuid`
    pub location_id: uuid::Uuid,
    /// `text`
    pub pallet_code: String,
    /// `int64`
    pub row_version: i64,
    /// `text`
    pub status: String,
    /// `timestamptz`
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Input descriptors for `wamn-wms:pallet/query@1.0.0`.
pub const PALLET_QUERY_INPUT: &[FieldDescriptor] = &[
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
        path: "filter.pallet_code[]",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "filter.status[]",
        type_name: "text",
        nullable: false,
        values: &[
            "available",
            "consumed",
            "held",
        ],
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
            "location_id",
            "pallet_code",
            "updated_at",
        ],
    },
];

/// Result descriptors for `wamn-wms:pallet/query@1.0.0`.
pub const PALLET_QUERY_RESULT: &[FieldDescriptor] = &[
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
        path: "location_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "pallet_code",
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
            "available",
            "consumed",
            "held",
        ],
    },
    FieldDescriptor {
        path: "updated_at",
        type_name: "timestamptz",
        nullable: false,
        values: &[],
    },
];

pub const PALLET_QUERY_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "cursor", type_name: "text", nullable: false, values: &[] },
required: false, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "filter", type_name: "object", nullable: false, values: &[] },
required: false, minimum: None, maximum: None, children: &[
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "filter.location_id[]", type_name: "array", nullable: false, values: &[] },
required: false, minimum: None, maximum: None, children: &[
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "filter.location_id[]", type_name: "uuid", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "filter.pallet_code[]", type_name: "array", nullable: false, values: &[] },
required: false, minimum: None, maximum: None, children: &[
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "filter.pallet_code[]", type_name: "text", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "filter.status[]", type_name: "array", nullable: false, values: &[] },
required: false, minimum: None, maximum: None, children: &[
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "filter.status[]", type_name: "text", nullable: false, values: &["available", "consumed", "held"] },
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
field: FieldDescriptor { path: "sort.field", type_name: "text", nullable: false, values: &["created_at", "location_id", "pallet_code", "updated_at"] },
required: true, minimum: None, maximum: None, children: &[
], },
], },
];

pub const PALLET_QUERY_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "created_at", type_name: "timestamptz", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "id", type_name: "uuid", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "location_id", type_name: "uuid", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "pallet_code", type_name: "text", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "row_version", type_name: "int64", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "status", type_name: "text", nullable: false, values: &["available", "consumed", "held"] },
required: true, minimum: None, maximum: None, children: &[
], },
wamn_client::descriptor::FieldSchema {
field: FieldDescriptor { path: "updated_at", type_name: "timestamptz", nullable: false, values: &[] },
required: true, minimum: None, maximum: None, children: &[
], },
];

pub const PALLET_QUERY_KIND: &str = "query";
pub const PALLET_QUERY_REQUIRES_COMPOSITION: bool = false;
pub const PALLET_QUERY_REPLAY: Option<&str> = None;
pub const PALLET_QUERY_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const PALLET_QUERY_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `wamn-wms:pallet/query@1.0.0`.
pub const PALLET_QUERY_GRANT: &str = "wamn-wms:pallet/query@1.0.0";

/// Typed refusals `wamn-wms:pallet/query@1.0.0` declares.
pub const PALLET_QUERY_ERRORS: &[&str] = &[
    "internal_error",
    "invalid_input",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `wamn-wms:pallet/query@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn query_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/pallet/query".to_owned(),
    }
}

/// Invoke `wamn-wms:pallet/query@1.0.0` through a bound client.
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
