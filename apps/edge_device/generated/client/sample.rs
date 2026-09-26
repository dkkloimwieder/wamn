// @generated from the client-contract IR; do not edit.
//!
//! `sample` operations of package `edge_device`.

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
        path: "frame",
        type_name: "text",
        nullable: false,
        values: &[],
    },
];

/// Input for `edge-device:sample/read@1.0.0`.
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

/// Result of `edge-device:sample/read@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct SampleReadResult {
    /// `timestamptz`
    pub captured_at: chrono::DateTime<chrono::Utc>,
    /// `text`
    pub frame: String,
}

/// Input descriptors for `edge-device:sample/read@1.0.0`.
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

/// Result descriptors for `edge-device:sample/read@1.0.0`.
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
pub const SAMPLE_READ_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const SAMPLE_READ_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `edge-device:sample/read@1.0.0`.
pub const SAMPLE_READ_GRANT: &str = "edge-device:sample/read@1.0.0";

/// Typed refusals `edge-device:sample/read@1.0.0` declares.
pub const SAMPLE_READ_ERRORS: &[&str] = &["internal_error", "invalid_input", "permission_denied"];

/// Where the release publishes `edge-device:sample/read@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn read_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/sample/read".to_owned(),
    }
}

/// Invoke `edge-device:sample/read@1.0.0` through a bound client.
///
/// # Errors
///
/// [`ClientError`] for a transport failure, a refusal, or a response that
/// does not match the operation's envelope.
pub async fn read(
    client: &WamnClient,
    items: &[serde_json::Value],
) -> Result<Vec<wamn_client::ItemOutcome>, ClientError> {
    client
        .invoke(&read_route(), &std::collections::BTreeMap::new(), items)
        .await
}
