// @generated from the client-contract IR; do not edit.
//!
//! `widget_tag` operations of package `platform_fixture`.

use wamn_client::{ClientError, FieldDescriptor, RouteMetadata, WamnClient};

/// Every field the `widget_tag` model projects.
pub const WIDGET_TAG_FIELDS: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "edit_version",
        type_name: "int64",
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
        path: "label",
        type_name: "text",
        nullable: false,
        values: &[],
    },
];

/// Input for `platform-fixture:widget-tag/update@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetTagUpdateRequest {
    /// `object`
    pub change: WidgetTagUpdateRequestChange,
    /// `int64`
    pub expected_edit_version: i64,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `string`
    pub request_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WidgetTagUpdateRequestChange {
    /// `text`, omittable
    pub label: Option<String>,
}

/// Result of `platform-fixture:widget-tag/update@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetTagUpdateResult {
    /// `int64`
    pub edit_version: i64,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `text`
    pub label: String,
}

/// Input descriptors for `platform-fixture:widget-tag/update@1.0.0`.
pub const WIDGET_TAG_UPDATE_INPUT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "change.label",
        type_name: "text",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "expected_edit_version",
        type_name: "int64",
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

/// Result descriptors for `platform-fixture:widget-tag/update@1.0.0`.
pub const WIDGET_TAG_UPDATE_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "edit_version",
        type_name: "int64",
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
        path: "label",
        type_name: "text",
        nullable: false,
        values: &[],
    },
];

pub const WIDGET_TAG_UPDATE_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
                path: "change.label",
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
            path: "expected_edit_version",
            type_name: "int64",
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

pub const WIDGET_TAG_UPDATE_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "edit_version",
            type_name: "int64",
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
            path: "label",
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

pub const WIDGET_TAG_UPDATE_KIND: &str = "update";
pub const WIDGET_TAG_UPDATE_REQUIRES_COMPOSITION: bool = true;
pub const WIDGET_TAG_UPDATE_REPLAY: Option<&str> = None;
pub const WIDGET_TAG_UPDATE_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const WIDGET_TAG_UPDATE_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `platform-fixture:widget-tag/update@1.0.0`.
pub const WIDGET_TAG_UPDATE_GRANT: &str = "platform-fixture:widget-tag/update@1.0.0";

/// Typed refusals `platform-fixture:widget-tag/update@1.0.0` declares.
pub const WIDGET_TAG_UPDATE_ERRORS: &[&str] = &[
    "concurrency_conflict",
    "internal_error",
    "invalid_input",
    "not_found",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `platform-fixture:widget-tag/update@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn update_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/widget_tag/update".to_owned(),
    }
}

/// Invoke `platform-fixture:widget-tag/update@1.0.0` through a bound client.
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
