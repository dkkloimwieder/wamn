// @generated from the client-contract IR; do not edit.
//!
//! `receipt` operations of package `wamn_receiving`.

use wamn_client::{ClientError, FieldDescriptor, RouteMetadata, WamnClient};

/// Every field the `receipt` model projects.
pub const RECEIPT_FIELDS: &[FieldDescriptor] = &[
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
        path: "idempotency_key",
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
        path: "purchase_order_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "receipt_reference",
        type_name: "text",
        nullable: false,
        values: &[],
    },
];


/// Input for `wamn-receiving:receipt/get@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct ReceiptGetRequest {
    /// `uuid`
    pub id: uuid::Uuid,
    /// `string`
    pub request_id: String,
}

/// Result of `wamn-receiving:receipt/get@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct ReceiptGetResult {
    /// `timestamptz`
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `text`
    pub idempotency_key: String,
    /// `timestamptz`
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    /// `uuid`
    pub purchase_order_id: uuid::Uuid,
    /// `text`
    pub receipt_reference: String,
}

/// Input descriptors for `wamn-receiving:receipt/get@1.0.0`.
pub const RECEIPT_GET_INPUT: &[FieldDescriptor] = &[
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

/// Result descriptors for `wamn-receiving:receipt/get@1.0.0`.
pub const RECEIPT_GET_RESULT: &[FieldDescriptor] = &[
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
        path: "idempotency_key",
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
        path: "purchase_order_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "receipt_reference",
        type_name: "text",
        nullable: false,
        values: &[],
    },
];

/// The grant a caller presents to invoke `wamn-receiving:receipt/get@1.0.0`.
pub const RECEIPT_GET_GRANT: &str = "wamn-receiving:receipt/get@1.0.0";

/// Typed refusals `wamn-receiving:receipt/get@1.0.0` declares.
pub const RECEIPT_GET_ERRORS: &[&str] = &[
    "internal_error",
    "invalid_input",
    "not_found",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `wamn-receiving:receipt/get@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn get_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/receipt/get".to_owned(),
    }
}

/// Invoke `wamn-receiving:receipt/get@1.0.0` through a bound client.
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


/// Input for `wamn-receiving:receipt/query@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct ReceiptQueryRequest {
    /// `string`
    pub request_id: String,
}

/// Result of `wamn-receiving:receipt/query@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct ReceiptQueryResult {
    /// `timestamptz`
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `text`
    pub idempotency_key: String,
    /// `timestamptz`
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    /// `uuid`
    pub purchase_order_id: uuid::Uuid,
    /// `text`
    pub receipt_reference: String,
}

/// Input descriptors for `wamn-receiving:receipt/query@1.0.0`.
pub const RECEIPT_QUERY_INPUT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "request_id",
        type_name: "string",
        nullable: false,
        values: &[],
    },
];

/// Result descriptors for `wamn-receiving:receipt/query@1.0.0`.
pub const RECEIPT_QUERY_RESULT: &[FieldDescriptor] = &[
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
        path: "idempotency_key",
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
        path: "purchase_order_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "receipt_reference",
        type_name: "text",
        nullable: false,
        values: &[],
    },
];

/// The grant a caller presents to invoke `wamn-receiving:receipt/query@1.0.0`.
pub const RECEIPT_QUERY_GRANT: &str = "wamn-receiving:receipt/query@1.0.0";

/// Typed refusals `wamn-receiving:receipt/query@1.0.0` declares.
pub const RECEIPT_QUERY_ERRORS: &[&str] = &[
    "internal_error",
    "invalid_input",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `wamn-receiving:receipt/query@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn query_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/receipt/query".to_owned(),
    }
}

/// Invoke `wamn-receiving:receipt/query@1.0.0` through a bound client.
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
