// @generated from the client-contract IR; do not edit.
//!
//! `receiving` operations of package `wamn_receiving`.

use wamn_client::{ClientError, FieldDescriptor, RouteMetadata, WamnClient};

/// Every field the `receiving` model projects.
pub const RECEIVING_FIELDS: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "item_id",
        type_name: "uuid",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "item_number",
        type_name: "text",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "line_id",
        type_name: "uuid",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "line_number",
        type_name: "int32",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "ordered_quantity",
        type_name: "numeric",
        nullable: true,
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
        path: "purchase_order_status",
        type_name: "text",
        nullable: false,
        values: &[
            "cancelled",
            "complete",
            "open",
        ],
    },
    FieldDescriptor {
        path: "receipt_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "received_quantity",
        type_name: "numeric",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "remaining_quantity",
        type_name: "numeric",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "row_version",
        type_name: "int64",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "supplier_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
];


/// Input for `wamn-receiving:receiving/load-receipt-screen@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct ReceivingLoadReceiptScreenRequest {
    /// `uuid`
    pub purchase_order_id: uuid::Uuid,
    /// `text`
    pub request_id: String,
}

/// Result of `wamn-receiving:receiving/load-receipt-screen@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct ReceivingLoadReceiptScreenResult {
    /// `uuid`, optional
    pub item_id: Option<uuid::Uuid>,
    /// `text`, optional
    pub item_number: Option<String>,
    /// `uuid`, optional
    pub line_id: Option<uuid::Uuid>,
    /// `int32`, optional
    pub line_number: Option<i32>,
    /// `numeric`, optional
    pub ordered_quantity: Option<rust_decimal::Decimal>,
    /// `uuid`
    pub purchase_order_id: uuid::Uuid,
    /// `text`
    pub purchase_order_number: String,
    /// `text`
    pub purchase_order_status: String,
    /// `numeric`, optional
    pub received_quantity: Option<rust_decimal::Decimal>,
    /// `numeric`, optional
    pub remaining_quantity: Option<rust_decimal::Decimal>,
    /// `int64`
    pub row_version: i64,
    /// `uuid`
    pub supplier_id: uuid::Uuid,
}

/// Input descriptors for `wamn-receiving:receiving/load-receipt-screen@1.0.0`.
pub const RECEIVING_LOAD_RECEIPT_SCREEN_INPUT: &[FieldDescriptor] = &[
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

/// Result descriptors for `wamn-receiving:receiving/load-receipt-screen@1.0.0`.
pub const RECEIVING_LOAD_RECEIPT_SCREEN_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "item_id",
        type_name: "uuid",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "item_number",
        type_name: "text",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "line_id",
        type_name: "uuid",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "line_number",
        type_name: "int32",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "ordered_quantity",
        type_name: "numeric",
        nullable: true,
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
        path: "purchase_order_status",
        type_name: "text",
        nullable: false,
        values: &[
            "cancelled",
            "complete",
            "open",
        ],
    },
    FieldDescriptor {
        path: "received_quantity",
        type_name: "numeric",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "remaining_quantity",
        type_name: "numeric",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "row_version",
        type_name: "int64",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "supplier_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
];

/// The grant a caller presents to invoke `wamn-receiving:receiving/load-receipt-screen@1.0.0`.
pub const RECEIVING_LOAD_RECEIPT_SCREEN_GRANT: &str = "wamn-receiving:receiving/load-receipt-screen@1.0.0";

/// Typed refusals `wamn-receiving:receiving/load-receipt-screen@1.0.0` declares.
pub const RECEIVING_LOAD_RECEIPT_SCREEN_ERRORS: &[&str] = &[
    "internal_error",
    "invalid_input",
    "not_found",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `wamn-receiving:receiving/load-receipt-screen@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn load_receipt_screen_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/receiving/load_receipt_screen".to_owned(),
    }
}

/// Invoke `wamn-receiving:receiving/load-receipt-screen@1.0.0` through a bound client.
///
/// # Errors
///
/// [`ClientError`] for a transport failure, a refusal, or a response that
/// does not match the operation's envelope.
pub async fn load_receipt_screen(
    client: &WamnClient,
    items: &[serde_json::Value],
) -> Result<Vec<wamn_client::ItemOutcome>, ClientError> {
    client
        .invoke(&load_receipt_screen_route(), &std::collections::BTreeMap::new(), items)
        .await
}


/// Input for `wamn-receiving:receiving/record-receipt@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct ReceivingRecordReceiptRequest {
    /// `text`
    pub request_id: String,
}

/// Result of `wamn-receiving:receiving/record-receipt@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct ReceivingRecordReceiptResult {
    /// `uuid`
    pub purchase_order_id: uuid::Uuid,
    /// `text`
    pub purchase_order_status: String,
    /// `uuid`
    pub receipt_id: uuid::Uuid,
    /// `int64`
    pub row_version: i64,
}

/// Input descriptors for `wamn-receiving:receiving/record-receipt@1.0.0`.
pub const RECEIVING_RECORD_RECEIPT_INPUT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "request_id",
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
        path: "value.line[].location_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.line[].purchase_order_line_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.line[].quantity",
        type_name: "numeric",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.occurred_at",
        type_name: "timestamptz",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.purchase_order_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.receipt_reference",
        type_name: "text",
        nullable: false,
        values: &[],
    },
];

/// Result descriptors for `wamn-receiving:receiving/record-receipt@1.0.0`.
pub const RECEIVING_RECORD_RECEIPT_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "purchase_order_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "purchase_order_status",
        type_name: "text",
        nullable: false,
        values: &[
            "complete",
            "open",
        ],
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
];

/// The grant a caller presents to invoke `wamn-receiving:receiving/record-receipt@1.0.0`.
pub const RECEIVING_RECORD_RECEIPT_GRANT: &str = "wamn-receiving:receiving/record-receipt@1.0.0";

/// Typed refusals `wamn-receiving:receiving/record-receipt@1.0.0` declares.
pub const RECEIVING_RECORD_RECEIPT_ERRORS: &[&str] = &[
    "idempotency_conflict",
    "internal_error",
    "invalid_input",
    "location_not_found",
    "permission_denied",
    "purchase_order_line_mismatch",
    "purchase_order_line_not_found",
    "purchase_order_not_found",
    "purchase_order_not_open",
    "quantity_exceeds_remaining",
    "receipt_reference_conflict",
    "retry",
    "timeout",
];

/// Where the release publishes `wamn-receiving:receiving/record-receipt@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn record_receipt_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/receiving/record_receipt".to_owned(),
    }
}

/// Invoke `wamn-receiving:receiving/record-receipt@1.0.0` through a bound client.
///
/// # Errors
///
/// [`ClientError`] for a transport failure, a refusal, or a response that
/// does not match the operation's envelope.
pub async fn record_receipt(
    client: &WamnClient,
    items: &[serde_json::Value],
) -> Result<Vec<wamn_client::ItemOutcome>, ClientError> {
    client
        .invoke(&record_receipt_route(), &std::collections::BTreeMap::new(), items)
        .await
}
