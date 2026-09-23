// @generated from the client-contract IR; do not edit.
//!
//! `widget` operations of package `platform_fixture`.

use wamn_client::{ClientError, FieldDescriptor, RouteMetadata, WamnClient};

/// Every field the `widget` model projects.
pub const WIDGET_FIELDS: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "attributes",
        type_name: "json",
        nullable: false,
        values: &[],
    },
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
        path: "outcome",
        type_name: "text",
        nullable: true,
        values: &["deleted"],
    },
    FieldDescriptor {
        path: "widget_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
];

/// Input for `platform-fixture:widget/archive@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetArchiveRequest {
    /// `int64`
    pub expected_edit_version: i64,
    /// `uuid`
    pub id: uuid::Uuid,
}

/// Result of `platform-fixture:widget/archive@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetArchiveResult {
    /// `int64`
    pub edit_version: i64,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `text`
    pub note: Option<String>,
}

/// Input descriptors for `platform-fixture:widget/archive@1.0.0`.
pub const WIDGET_ARCHIVE_INPUT: &[FieldDescriptor] = &[
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
];

/// Result descriptors for `platform-fixture:widget/archive@1.0.0`.
pub const WIDGET_ARCHIVE_RESULT: &[FieldDescriptor] = &[
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
        path: "note",
        type_name: "text",
        nullable: true,
        values: &[],
    },
];

pub const WIDGET_ARCHIVE_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
];

pub const WIDGET_ARCHIVE_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
];

pub const WIDGET_ARCHIVE_KIND: &str = "command";
pub const WIDGET_ARCHIVE_REQUIRES_COMPOSITION: bool = true;
pub const WIDGET_ARCHIVE_REPLAY: Option<&str> = Some("state");
pub const WIDGET_ARCHIVE_RESPONSE_CONTRACT: Option<&str> = None;
pub const WIDGET_ARCHIVE_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `platform-fixture:widget/archive@1.0.0`.
pub const WIDGET_ARCHIVE_GRANT: &str = "platform-fixture:widget/archive@1.0.0";

/// Typed refusals `platform-fixture:widget/archive@1.0.0` declares.
pub const WIDGET_ARCHIVE_ERRORS: &[&str] = &[
    "already_archived",
    "concurrency_conflict",
    "internal_error",
    "invalid_input",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `platform-fixture:widget/archive@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn archive_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/widget/archive".to_owned(),
    }
}

/// Invoke `platform-fixture:widget/archive@1.0.0` through a bound client.
///
/// # Errors
///
/// [`ClientError`] for a transport failure, a refusal, or a response that
/// does not match the operation's envelope.
pub async fn archive(
    client: &WamnClient,
    items: &[serde_json::Value],
) -> Result<Vec<wamn_client::ItemOutcome>, ClientError> {
    client
        .invoke(&archive_route(), &std::collections::BTreeMap::new(), items)
        .await
}

/// Input for `platform-fixture:widget/create@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetCreateRequest {
    /// `text`, omittable
    pub code: Option<String>,
    /// `text`
    pub idempotency_key: String,
    /// `uuid`, omittable
    pub maker_id: Option<Option<uuid::Uuid>>,
    /// `text`, omittable
    pub note: Option<Option<String>>,
    /// `string`
    pub request_id: String,
}

/// Result of `platform-fixture:widget/create@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetCreateResult {
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
}

/// Input descriptors for `platform-fixture:widget/create@1.0.0`.
pub const WIDGET_CREATE_INPUT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "code",
        type_name: "text",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "idempotency_key",
        type_name: "text",
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
        path: "request_id",
        type_name: "string",
        nullable: false,
        values: &[],
    },
];

/// Result descriptors for `platform-fixture:widget/create@1.0.0`.
pub const WIDGET_CREATE_RESULT: &[FieldDescriptor] = &[
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
];

pub const WIDGET_CREATE_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "code",
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
            path: "maker_id",
            type_name: "uuid",
            nullable: true,
            values: &[],
        },
        required: false,
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
        required: false,
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

pub const WIDGET_CREATE_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
];

pub const WIDGET_CREATE_KIND: &str = "create";
pub const WIDGET_CREATE_REQUIRES_COMPOSITION: bool = false;
pub const WIDGET_CREATE_REPLAY: Option<&str> = Some("claim");
pub const WIDGET_CREATE_RESPONSE_CONTRACT: Option<&str> = None;
pub const WIDGET_CREATE_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `platform-fixture:widget/create@1.0.0`.
pub const WIDGET_CREATE_GRANT: &str = "platform-fixture:widget/create@1.0.0";

/// Typed refusals `platform-fixture:widget/create@1.0.0` declares.
pub const WIDGET_CREATE_ERRORS: &[&str] = &[
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

/// Where the release publishes `platform-fixture:widget/create@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn create_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/widget/create".to_owned(),
    }
}

/// Invoke `platform-fixture:widget/create@1.0.0` through a bound client.
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

/// Input for `platform-fixture:widget/delete@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetDeleteRequest {
    /// `int64`
    pub expected_edit_version: i64,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `string`
    pub request_id: String,
}

/// Result of `platform-fixture:widget/delete@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetDeleteResult {
    /// `text`
    pub outcome: Option<String>,
}

/// Input descriptors for `platform-fixture:widget/delete@1.0.0`.
pub const WIDGET_DELETE_INPUT: &[FieldDescriptor] = &[
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

/// Result descriptors for `platform-fixture:widget/delete@1.0.0`.
pub const WIDGET_DELETE_RESULT: &[FieldDescriptor] = &[FieldDescriptor {
    path: "outcome",
    type_name: "text",
    nullable: true,
    values: &["deleted"],
}];

pub const WIDGET_DELETE_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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

pub const WIDGET_DELETE_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] =
    &[wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "outcome",
            type_name: "text",
            nullable: true,
            values: &["deleted"],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    }];

pub const WIDGET_DELETE_KIND: &str = "delete";
pub const WIDGET_DELETE_REQUIRES_COMPOSITION: bool = false;
pub const WIDGET_DELETE_REPLAY: Option<&str> = None;
pub const WIDGET_DELETE_RESPONSE_CONTRACT: Option<&str> = None;
pub const WIDGET_DELETE_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `platform-fixture:widget/delete@1.0.0`.
pub const WIDGET_DELETE_GRANT: &str = "platform-fixture:widget/delete@1.0.0";

/// Typed refusals `platform-fixture:widget/delete@1.0.0` declares.
pub const WIDGET_DELETE_ERRORS: &[&str] = &[
    "concurrency_conflict",
    "internal_error",
    "invalid_input",
    "not_found",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `platform-fixture:widget/delete@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn delete_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/widget/delete".to_owned(),
    }
}

/// Invoke `platform-fixture:widget/delete@1.0.0` through a bound client.
///
/// # Errors
///
/// [`ClientError`] for a transport failure, a refusal, or a response that
/// does not match the operation's envelope.
pub async fn delete(
    client: &WamnClient,
    items: &[serde_json::Value],
) -> Result<Vec<wamn_client::ItemOutcome>, ClientError> {
    client
        .invoke(&delete_route(), &std::collections::BTreeMap::new(), items)
        .await
}

/// Input for `platform-fixture:widget/get@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetGetRequest {
    /// `uuid`
    pub id: uuid::Uuid,
    /// `string`
    pub request_id: String,
}

/// Result of `platform-fixture:widget/get@1.0.0`.
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
}

/// Input descriptors for `platform-fixture:widget/get@1.0.0`.
pub const WIDGET_GET_INPUT: &[FieldDescriptor] = &[
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

/// Result descriptors for `platform-fixture:widget/get@1.0.0`.
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
];

pub const WIDGET_GET_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
];

pub const WIDGET_GET_KIND: &str = "get";
pub const WIDGET_GET_REQUIRES_COMPOSITION: bool = false;
pub const WIDGET_GET_REPLAY: Option<&str> = None;
pub const WIDGET_GET_RESPONSE_CONTRACT: Option<&str> = None;
pub const WIDGET_GET_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `platform-fixture:widget/get@1.0.0`.
pub const WIDGET_GET_GRANT: &str = "platform-fixture:widget/get@1.0.0";

/// Typed refusals `platform-fixture:widget/get@1.0.0` declares.
pub const WIDGET_GET_ERRORS: &[&str] = &[
    "internal_error",
    "invalid_input",
    "not_found",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `platform-fixture:widget/get@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn get_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/widget/get".to_owned(),
    }
}

/// Invoke `platform-fixture:widget/get@1.0.0` through a bound client.
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

/// Input for `platform-fixture:widget/list@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetListRequest {
    /// `uuid`
    pub maker_id: Option<uuid::Uuid>,
    /// `text`
    pub request_id: String,
    /// `json`
    pub selector: serde_json::Value,
}

/// Result of `platform-fixture:widget/list@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetListResult {
    /// `json`
    pub attributes: serde_json::Value,
    /// `text`
    pub code: String,
    /// `int64`
    pub edit_version: i64,
    /// `uuid`
    pub id: uuid::Uuid,
}

/// Input descriptors for `platform-fixture:widget/list@1.0.0`.
pub const WIDGET_LIST_INPUT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "maker_id",
        type_name: "uuid",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "request_id",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "selector",
        type_name: "json",
        nullable: false,
        values: &[],
    },
];

/// Result descriptors for `platform-fixture:widget/list@1.0.0`.
pub const WIDGET_LIST_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "attributes",
        type_name: "json",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "code",
        type_name: "text",
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
];

pub const WIDGET_LIST_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
            path: "selector",
            type_name: "json",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    },
];

pub const WIDGET_LIST_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "attributes",
            type_name: "json",
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
];

pub const WIDGET_LIST_KIND: &str = "projection";
pub const WIDGET_LIST_REQUIRES_COMPOSITION: bool = false;
pub const WIDGET_LIST_REPLAY: Option<&str> = None;
pub const WIDGET_LIST_RESPONSE_CONTRACT: Option<&str> = None;
pub const WIDGET_LIST_RESULT_OPAQUE: bool = true;
/// The grant a caller presents to invoke `platform-fixture:widget/list@1.0.0`.
pub const WIDGET_LIST_GRANT: &str = "platform-fixture:widget/list@1.0.0";

/// Typed refusals `platform-fixture:widget/list@1.0.0` declares.
pub const WIDGET_LIST_ERRORS: &[&str] = &[
    "internal_error",
    "invalid_input",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `platform-fixture:widget/list@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn list_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/widget/list".to_owned(),
    }
}

/// Invoke `platform-fixture:widget/list@1.0.0` through a bound client.
///
/// # Errors
///
/// [`ClientError`] for a transport failure, a refusal, or a response that
/// does not match the operation's envelope.
pub async fn list(
    client: &WamnClient,
    items: &[serde_json::Value],
) -> Result<Vec<wamn_client::ItemOutcome>, ClientError> {
    client
        .invoke(&list_route(), &std::collections::BTreeMap::new(), items)
        .await
}

/// Input for `platform-fixture:widget/query@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetQueryRequest {
    /// `text`, omittable
    pub cursor: Option<String>,
    /// `object`, omittable
    pub filter: Option<WidgetQueryRequestFilter>,
    /// `int32`, omittable
    pub limit: Option<i32>,
    /// `string`
    pub request_id: String,
    /// `object`, omittable
    pub sort: Option<WidgetQueryRequestSort>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WidgetQueryRequestFilter {
    /// `array`, omittable
    pub code: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WidgetQueryRequestSort {
    /// `text`
    pub direction: String,
    /// `text`
    pub field: String,
}

/// Result of `platform-fixture:widget/query@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetQueryResult {
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
}

/// Input descriptors for `platform-fixture:widget/query@1.0.0`.
pub const WIDGET_QUERY_INPUT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "cursor",
        type_name: "text",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "filter.code[]",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "limit",
        type_name: "int32",
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
        values: &[],
    },
    FieldDescriptor {
        path: "sort.field",
        type_name: "text",
        nullable: false,
        values: &[],
    },
];

/// Result descriptors for `platform-fixture:widget/query@1.0.0`.
pub const WIDGET_QUERY_RESULT: &[FieldDescriptor] = &[
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
];

pub const WIDGET_QUERY_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
        children: &[wamn_client::descriptor::FieldSchema {
            field: FieldDescriptor {
                path: "filter.code[]",
                type_name: "array",
                nullable: false,
                values: &[],
            },
            required: false,
            minimum: None,
            maximum: None,
            children: &[wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "filter.code[]",
                    type_name: "text",
                    nullable: false,
                    values: &[],
                },
                required: true,
                minimum: None,
                maximum: None,
                children: &[],
            }],
        }],
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
                    values: &[],
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

pub const WIDGET_QUERY_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
];

pub const WIDGET_QUERY_KIND: &str = "query";
pub const WIDGET_QUERY_REQUIRES_COMPOSITION: bool = false;
pub const WIDGET_QUERY_REPLAY: Option<&str> = None;
pub const WIDGET_QUERY_RESPONSE_CONTRACT: Option<&str> = None;
pub const WIDGET_QUERY_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `platform-fixture:widget/query@1.0.0`.
pub const WIDGET_QUERY_GRANT: &str = "platform-fixture:widget/query@1.0.0";

/// Typed refusals `platform-fixture:widget/query@1.0.0` declares.
pub const WIDGET_QUERY_ERRORS: &[&str] = &[
    "internal_error",
    "invalid_input",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `platform-fixture:widget/query@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn query_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/widget/query".to_owned(),
    }
}

/// Invoke `platform-fixture:widget/query@1.0.0` through a bound client.
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

/// Input for `platform-fixture:widget/record-batch@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetRecordBatchRequest {
    /// `text`
    pub request_id: String,
    /// `object`
    pub value: WidgetRecordBatchRequestValue,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WidgetRecordBatchRequestValue {
    /// `text`
    pub idempotency_key: String,
    /// `array`
    pub line: Vec<WidgetRecordBatchRequestValueLine>,
    /// `uuid`, omittable
    pub maker_id: Option<Option<uuid::Uuid>>,
    /// `text`, omittable
    pub note: Option<Option<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WidgetRecordBatchRequestValueLine {
    /// `numeric`
    pub amount: rust_decimal::Decimal,
    /// `uuid`
    pub widget_id: uuid::Uuid,
}

/// Result of `platform-fixture:widget/record-batch@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetRecordBatchResult {
    /// `uuid`
    pub widget_id: uuid::Uuid,
}

/// Input descriptors for `platform-fixture:widget/record-batch@1.0.0`.
pub const WIDGET_RECORD_BATCH_INPUT: &[FieldDescriptor] = &[
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
        path: "value.line[].amount",
        type_name: "numeric",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.line[].widget_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "value.maker_id",
        type_name: "uuid",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "value.note",
        type_name: "text",
        nullable: true,
        values: &[],
    },
];

/// Result descriptors for `platform-fixture:widget/record-batch@1.0.0`.
pub const WIDGET_RECORD_BATCH_RESULT: &[FieldDescriptor] = &[FieldDescriptor {
    path: "widget_id",
    type_name: "uuid",
    nullable: false,
    values: &[],
}];

pub const WIDGET_RECORD_BATCH_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
                    path: "value.line[]",
                    type_name: "array",
                    nullable: false,
                    values: &[],
                },
                required: true,
                minimum: Some(1),
                maximum: Some(10),
                children: &[
                    wamn_client::descriptor::FieldSchema {
                        field: FieldDescriptor {
                            path: "value.line[].amount",
                            type_name: "numeric",
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
                            path: "value.line[].widget_id",
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
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "value.maker_id",
                    type_name: "uuid",
                    nullable: true,
                    values: &[],
                },
                required: false,
                minimum: None,
                maximum: None,
                children: &[],
            },
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "value.note",
                    type_name: "text",
                    nullable: true,
                    values: &[],
                },
                required: false,
                minimum: None,
                maximum: None,
                children: &[],
            },
        ],
    },
];

pub const WIDGET_RECORD_BATCH_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] =
    &[wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "widget_id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        required: true,
        minimum: None,
        maximum: None,
        children: &[],
    }];

pub const WIDGET_RECORD_BATCH_KIND: &str = "command";
pub const WIDGET_RECORD_BATCH_REQUIRES_COMPOSITION: bool = false;
pub const WIDGET_RECORD_BATCH_REPLAY: Option<&str> = Some("claim");
pub const WIDGET_RECORD_BATCH_RESPONSE_CONTRACT: Option<&str> = None;
pub const WIDGET_RECORD_BATCH_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `platform-fixture:widget/record-batch@1.0.0`.
pub const WIDGET_RECORD_BATCH_GRANT: &str = "platform-fixture:widget/record-batch@1.0.0";

/// Typed refusals `platform-fixture:widget/record-batch@1.0.0` declares.
pub const WIDGET_RECORD_BATCH_ERRORS: &[&str] = &[
    "idempotency_conflict",
    "internal_error",
    "invalid_input",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `platform-fixture:widget/record-batch@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn record_batch_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/widget/record_batch".to_owned(),
    }
}

/// Invoke `platform-fixture:widget/record-batch@1.0.0` through a bound client.
///
/// # Errors
///
/// [`ClientError`] for a transport failure, a refusal, or a response that
/// does not match the operation's envelope.
pub async fn record_batch(
    client: &WamnClient,
    items: &[serde_json::Value],
) -> Result<Vec<wamn_client::ItemOutcome>, ClientError> {
    client
        .invoke(
            &record_batch_route(),
            &std::collections::BTreeMap::new(),
            items,
        )
        .await
}

/// Input for `platform-fixture:widget/update@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetUpdateRequest {
    /// `object`, omittable
    pub change: Option<WidgetUpdateRequestChange>,
    /// `int64`
    pub expected_edit_version: i64,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `string`
    pub request_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WidgetUpdateRequestChange {
    /// `text`, omittable
    pub code: Option<String>,
    /// `uuid`, omittable
    pub maker_id: Option<Option<uuid::Uuid>>,
    /// `text`, omittable
    pub note: Option<Option<String>>,
}

/// Result of `platform-fixture:widget/update@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetUpdateResult {
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
}

/// Input descriptors for `platform-fixture:widget/update@1.0.0`.
pub const WIDGET_UPDATE_INPUT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "change.code",
        type_name: "text",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "change.maker_id",
        type_name: "uuid",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "change.note",
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

/// Result descriptors for `platform-fixture:widget/update@1.0.0`.
pub const WIDGET_UPDATE_RESULT: &[FieldDescriptor] = &[
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
];

pub const WIDGET_UPDATE_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
    wamn_client::descriptor::FieldSchema {
        field: FieldDescriptor {
            path: "change",
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
                    path: "change.code",
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
                    path: "change.maker_id",
                    type_name: "uuid",
                    nullable: true,
                    values: &[],
                },
                required: false,
                minimum: None,
                maximum: None,
                children: &[],
            },
            wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "change.note",
                    type_name: "text",
                    nullable: true,
                    values: &[],
                },
                required: false,
                minimum: None,
                maximum: None,
                children: &[],
            },
        ],
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

pub const WIDGET_UPDATE_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
];

pub const WIDGET_UPDATE_KIND: &str = "update";
pub const WIDGET_UPDATE_REQUIRES_COMPOSITION: bool = false;
pub const WIDGET_UPDATE_REPLAY: Option<&str> = None;
pub const WIDGET_UPDATE_RESPONSE_CONTRACT: Option<&str> = None;
pub const WIDGET_UPDATE_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `platform-fixture:widget/update@1.0.0`.
pub const WIDGET_UPDATE_GRANT: &str = "platform-fixture:widget/update@1.0.0";

/// Typed refusals `platform-fixture:widget/update@1.0.0` declares.
pub const WIDGET_UPDATE_ERRORS: &[&str] = &[
    "concurrency_conflict",
    "foreign_key_violation",
    "internal_error",
    "invalid_input",
    "not_found",
    "permission_denied",
    "retry",
    "timeout",
    "unique_violation",
];

/// Where the release publishes `platform-fixture:widget/update@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn update_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/widget/update".to_owned(),
    }
}

/// Invoke `platform-fixture:widget/update@1.0.0` through a bound client.
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
