// @generated from the client-contract IR; do not edit.
//!
//! `widget` operations of package `platform_fixture_overlay`.

use wamn_client::{ClientError, FieldDescriptor, RouteMetadata, WamnClient};

/// Every field the `widget` model projects.
pub const WIDGET_FIELDS: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "code",
        type_name: "text",
        nullable: false,
        values: &["priority", "standard"],
    },
    FieldDescriptor {
        path: "created_at",
        type_name: "timestamptz",
        nullable: false,
        values: &[],
    },
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
        path: "maker_id",
        type_name: "uuid",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "note",
        type_name: "text",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "overlay_note",
        type_name: "text",
        nullable: true,
        values: &[],
    },
];

/// Input for `platform-fixture-overlay:widget/get@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetGetRequest {
    /// `uuid`
    pub id: uuid::Uuid,
}

/// Result of `platform-fixture-overlay:widget/get@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetGetResult {
    /// `text`
    pub code: String,
    /// `timestamptz`
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// `int64`
    pub edit_version: i64,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `uuid`
    pub maker_id: Option<uuid::Uuid>,
    /// `text`
    pub note: Option<String>,
    /// `text`
    pub overlay_note: Option<String>,
}

/// Input descriptors for `platform-fixture-overlay:widget/get@1.0.0`.
pub const WIDGET_GET_INPUT: &[FieldDescriptor] = &[FieldDescriptor {
    path: "id",
    type_name: "uuid",
    nullable: false,
    values: &[],
}];

/// Result descriptors for `platform-fixture-overlay:widget/get@1.0.0`.
pub const WIDGET_GET_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "code",
        type_name: "text",
        nullable: false,
        values: &["priority", "standard"],
    },
    FieldDescriptor {
        path: "created_at",
        type_name: "timestamptz",
        nullable: false,
        values: &[],
    },
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
        path: "maker_id",
        type_name: "uuid",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "note",
        type_name: "text",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "overlay_note",
        type_name: "text",
        nullable: true,
        values: &[],
    },
];

pub const WIDGET_GET_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] =
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

pub const WIDGET_GET_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "code",
            type_name: "text",
            nullable: false,
            values: &["priority", "standard"],
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
            path: "maker_id",
            type_name: "uuid",
            nullable: true,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "note",
            type_name: "text",
            nullable: true,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "overlay_note",
            type_name: "text",
            nullable: true,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
];

pub const WIDGET_GET_KIND: &str = "get";
pub const WIDGET_GET_REQUIRES_COMPOSITION: bool = false;
pub const WIDGET_GET_REPLAY: Option<&str> = None;
pub const WIDGET_GET_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const WIDGET_GET_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `platform-fixture-overlay:widget/get@1.0.0`.
pub const WIDGET_GET_GRANT: &str = "platform-fixture-overlay:widget/get@1.0.0";

/// Typed refusals `platform-fixture-overlay:widget/get@1.0.0` declares.
pub const WIDGET_GET_ERRORS: &[&str] = &[
    "internal_error",
    "invalid_input",
    "not_found",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `platform-fixture-overlay:widget/get@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn get_route() -> RouteMetadata {
    RouteMetadata {
        method: "GET".to_owned(),
        template: "/overlay/widget/get".to_owned(),
    }
}

/// Invoke `platform-fixture-overlay:widget/get@1.0.0` through a bound client.
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
