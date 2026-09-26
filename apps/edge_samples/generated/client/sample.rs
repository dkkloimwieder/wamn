// @generated from the client-contract IR; do not edit.
//!
//! `sample` operations of package `edge_samples`.

use wamn_client::{ClientError, FieldDescriptor, RouteMetadata, WamnClient};

/// Every field the `sample` model projects.
pub const SAMPLE_FIELDS: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "captured_at",
        type_name: "timestamptz",
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
        path: "frame",
        type_name: "text",
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
        path: "sample_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
];

/// Input for `edge-samples:sample/get@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct SampleGetRequest {
    /// `uuid`
    pub id: uuid::Uuid,
}

/// Result of `edge-samples:sample/get@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct SampleGetResult {
    /// `timestamptz`
    pub captured_at: chrono::DateTime<chrono::Utc>,
    /// `timestamptz`
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// `text`
    pub frame: String,
    /// `uuid`
    pub id: uuid::Uuid,
}

/// Input descriptors for `edge-samples:sample/get@1.0.0`.
pub const SAMPLE_GET_INPUT: &[FieldDescriptor] = &[FieldDescriptor {
    path: "id",
    type_name: "uuid",
    nullable: false,
    values: &[],
}];

/// Result descriptors for `edge-samples:sample/get@1.0.0`.
pub const SAMPLE_GET_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "captured_at",
        type_name: "timestamptz",
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
        path: "frame",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
];

pub const SAMPLE_GET_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] =
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

pub const SAMPLE_GET_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "captured_at",
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
            path: "frame",
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
];

pub const SAMPLE_GET_KIND: &str = "get";
pub const SAMPLE_GET_REQUIRES_COMPOSITION: bool = false;
pub const SAMPLE_GET_REPLAY: Option<&str> = None;
pub const SAMPLE_GET_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const SAMPLE_GET_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `edge-samples:sample/get@1.0.0`.
pub const SAMPLE_GET_GRANT: &str = "edge-samples:sample/get@1.0.0";

/// Typed refusals `edge-samples:sample/get@1.0.0` declares.
pub const SAMPLE_GET_ERRORS: &[&str] = &[
    "internal_error",
    "invalid_input",
    "not_found",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `edge-samples:sample/get@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn get_route() -> RouteMetadata {
    RouteMetadata {
        method: "GET".to_owned(),
        template: "/sample/get".to_owned(),
    }
}

/// Invoke `edge-samples:sample/get@1.0.0` through a bound client.
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

/// Input for `edge-samples:sample/read@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct SampleReadRequest {
    /// `text`
    pub request_id: String,
    /// `object`
    pub value: SampleReadRequestValue,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SampleReadRequestValue {
    /// `timestamptz`
    pub captured_at: chrono::DateTime<chrono::Utc>,
    /// `text`
    pub frame: String,
}

/// Result of `edge-samples:sample/read@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct SampleReadResult {
    /// `timestamptz`
    pub captured_at: chrono::DateTime<chrono::Utc>,
    /// `text`
    pub frame: String,
}

/// Input descriptors for `edge-samples:sample/read@1.0.0`.
pub const SAMPLE_READ_INPUT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "request_id",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.captured_at",
        type_name: "timestamptz",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.frame",
        type_name: "text",
        nullable: false,
        values: &[],
    },
];

/// Result descriptors for `edge-samples:sample/read@1.0.0`.
pub const SAMPLE_READ_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "captured_at",
        type_name: "timestamptz",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "frame",
        type_name: "text",
        nullable: false,
        values: &[],
    },
];

pub const SAMPLE_READ_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
                    path: "value.captured_at",
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
                    path: "value.frame",
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

pub const SAMPLE_READ_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "captured_at",
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
            path: "frame",
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

pub const SAMPLE_READ_KIND: &str = "command";
pub const SAMPLE_READ_REQUIRES_COMPOSITION: bool = false;
pub const SAMPLE_READ_REPLAY: Option<&str> = None;
pub const SAMPLE_READ_RESPONSE_CONTRACT: Option<&str> = None;
pub const SAMPLE_READ_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `edge-samples:sample/read@1.0.0`.
pub const SAMPLE_READ_GRANT: &str = "edge-samples:sample/read@1.0.0";

/// Typed refusals `edge-samples:sample/read@1.0.0` declares.
pub const SAMPLE_READ_ERRORS: &[&str] = &["internal_error", "invalid_input", "permission_denied"];

// `edge-samples:sample/read@1.0.0` is not published over HTTP by this release, so it has no route
// and no invoke function. It remains listed for its types and descriptors.

/// Input for `edge-samples:sample/record@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct SampleRecordRequest {
    /// `text`
    pub request_id: String,
    /// `object`
    pub value: SampleRecordRequestValue,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SampleRecordRequestValue {
    /// `timestamptz`
    pub captured_at: chrono::DateTime<chrono::Utc>,
    /// `text`
    pub frame: String,
    /// `text`
    pub idempotency_key: String,
}

/// Result of `edge-samples:sample/record@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct SampleRecordResult {
    /// `uuid`
    pub sample_id: uuid::Uuid,
}

/// Input descriptors for `edge-samples:sample/record@1.0.0`.
pub const SAMPLE_RECORD_INPUT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "request_id",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.captured_at",
        type_name: "timestamptz",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.frame",
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
];

/// Result descriptors for `edge-samples:sample/record@1.0.0`.
pub const SAMPLE_RECORD_RESULT: &[FieldDescriptor] = &[FieldDescriptor {
    path: "sample_id",
    type_name: "uuid",
    nullable: false,
    values: &[],
}];

pub const SAMPLE_RECORD_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
                    path: "value.captured_at",
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
                    path: "value.frame",
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
        ],
    },
];

pub const SAMPLE_RECORD_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] =
    &[wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "sample_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    }];

pub const SAMPLE_RECORD_KIND: &str = "command";
pub const SAMPLE_RECORD_REQUIRES_COMPOSITION: bool = false;
pub const SAMPLE_RECORD_REPLAY: Option<&str> = Some("claim");
pub const SAMPLE_RECORD_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const SAMPLE_RECORD_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `edge-samples:sample/record@1.0.0`.
pub const SAMPLE_RECORD_GRANT: &str = "edge-samples:sample/record@1.0.0";

/// Typed refusals `edge-samples:sample/record@1.0.0` declares.
pub const SAMPLE_RECORD_ERRORS: &[&str] = &[
    "idempotency_conflict",
    "internal_error",
    "invalid_input",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `edge-samples:sample/record@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn record_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/sample/record".to_owned(),
    }
}

/// Invoke `edge-samples:sample/record@1.0.0` through a bound client.
///
/// # Errors
///
/// [`ClientError`] for a transport failure, a refusal, or a response that
/// does not match the operation's envelope.
pub async fn record(
    client: &WamnClient,
    items: &[serde_json::Value],
) -> Result<Vec<wamn_client::ItemOutcome>, ClientError> {
    client
        .invoke(&record_route(), &std::collections::BTreeMap::new(), items)
        .await
}
