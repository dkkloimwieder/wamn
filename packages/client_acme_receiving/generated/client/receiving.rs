// @generated from the client-contract IR; do not edit.
//!
//! `receiving` operations of package `client_acme_receiving`.

use wamn_client::{ClientError, FieldDescriptor, RouteMetadata, WamnClient};

/// Every field the `receiving` model projects.
pub const RECEIVING_FIELDS: &[FieldDescriptor] = &[
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


/// Input for `client-acme-receiving:receiving/record-receipt@3.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct ReceivingRecordReceiptRequest {
    /// `text`
    pub request_id: String,
}

/// Result of `client-acme-receiving:receiving/record-receipt@3.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct ReceivingRecordReceiptResult {
    /// `boolean`
    pub acme_inspection_required: bool,
    /// `text`
    pub acme_quality_status: String,
    /// `uuid`
    pub purchase_order_id: uuid::Uuid,
    /// `text`
    pub purchase_order_status: String,
    /// `uuid`
    pub receipt_id: uuid::Uuid,
    /// `int64`
    pub row_version: i64,
}

/// Input descriptors for `client-acme-receiving:receiving/record-receipt@3.0.0`.
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

/// Result descriptors for `client-acme-receiving:receiving/record-receipt@3.0.0`.
pub const RECEIVING_RECORD_RECEIPT_RESULT: &[FieldDescriptor] = &[
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

/// The grant a caller presents to invoke `client-acme-receiving:receiving/record-receipt@3.0.0`.
pub const RECEIVING_RECORD_RECEIPT_GRANT: &str = "client-acme-receiving:receiving/record-receipt@3.0.0";

/// Typed refusals `client-acme-receiving:receiving/record-receipt@3.0.0` declares.
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

/// Where the release publishes `client-acme-receiving:receiving/record-receipt@3.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn record_receipt_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/acme/receiving/record_receipt".to_owned(),
    }
}

/// Invoke `client-acme-receiving:receiving/record-receipt@3.0.0` through a bound client.
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
