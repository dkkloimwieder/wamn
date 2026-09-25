// @generated from the client-contract IR; do not edit.
//!
//! `widget_maker` operations of package `platform_fixture`.

use wamn_client::{ClientError, FieldDescriptor, RouteMetadata, WamnClient};

/// Every field the `widget_maker` model projects.
pub const WIDGET_MAKER_FIELDS: &[FieldDescriptor] = &[
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
        path: "name",
        type_name: "text",
        nullable: false,
        values: &[],
    },
];

/// Input for `platform-fixture:widget-maker/get@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetMakerGetRequest {
    /// `uuid`
    pub id: uuid::Uuid,
}

/// Result of `platform-fixture:widget-maker/get@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetMakerGetResult {
    /// `timestamptz`
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// `int64`
    pub edit_version: i64,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `text`
    pub name: String,
}

/// Input descriptors for `platform-fixture:widget-maker/get@1.0.0`.
pub const WIDGET_MAKER_GET_INPUT: &[FieldDescriptor] = &[FieldDescriptor {
    path: "id",
    type_name: "uuid",
    nullable: false,
    values: &[],
}];

/// Result descriptors for `platform-fixture:widget-maker/get@1.0.0`.
pub const WIDGET_MAKER_GET_RESULT: &[FieldDescriptor] = &[
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
        path: "name",
        type_name: "text",
        nullable: false,
        values: &[],
    },
];

pub const WIDGET_MAKER_GET_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] =
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

pub const WIDGET_MAKER_GET_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
            path: "name",
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

pub const WIDGET_MAKER_GET_KIND: &str = "get";
pub const WIDGET_MAKER_GET_REQUIRES_COMPOSITION: bool = false;
pub const WIDGET_MAKER_GET_REPLAY: Option<&str> = None;
pub const WIDGET_MAKER_GET_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const WIDGET_MAKER_GET_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `platform-fixture:widget-maker/get@1.0.0`.
pub const WIDGET_MAKER_GET_GRANT: &str = "platform-fixture:widget-maker/get@1.0.0";

/// Typed refusals `platform-fixture:widget-maker/get@1.0.0` declares.
pub const WIDGET_MAKER_GET_ERRORS: &[&str] = &[
    "internal_error",
    "invalid_input",
    "not_found",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `platform-fixture:widget-maker/get@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn get_route() -> RouteMetadata {
    RouteMetadata {
        method: "GET".to_owned(),
        template: "/widget_maker/get".to_owned(),
    }
}

/// Invoke `platform-fixture:widget-maker/get@1.0.0` through a bound client.
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

/// Input for `platform-fixture:widget-maker/list@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetMakerListRequest {}

/// Result of `platform-fixture:widget-maker/list@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetMakerListResult {
    /// `uuid`
    pub id: uuid::Uuid,
    /// `text`
    pub name: String,
}

/// Input descriptors for `platform-fixture:widget-maker/list@1.0.0`.
pub const WIDGET_MAKER_LIST_INPUT: &[FieldDescriptor] = &[];

/// Result descriptors for `platform-fixture:widget-maker/list@1.0.0`.
pub const WIDGET_MAKER_LIST_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "name",
        type_name: "text",
        nullable: false,
        values: &[],
    },
];

pub const WIDGET_MAKER_LIST_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[];

pub const WIDGET_MAKER_LIST_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
            path: "name",
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

pub const WIDGET_MAKER_LIST_KIND: &str = "projection";
pub const WIDGET_MAKER_LIST_REQUIRES_COMPOSITION: bool = false;
pub const WIDGET_MAKER_LIST_REPLAY: Option<&str> = None;
pub const WIDGET_MAKER_LIST_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const WIDGET_MAKER_LIST_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `platform-fixture:widget-maker/list@1.0.0`.
pub const WIDGET_MAKER_LIST_GRANT: &str = "platform-fixture:widget-maker/list@1.0.0";

/// Typed refusals `platform-fixture:widget-maker/list@1.0.0` declares.
pub const WIDGET_MAKER_LIST_ERRORS: &[&str] = &[
    "internal_error",
    "invalid_input",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `platform-fixture:widget-maker/list@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn list_route() -> RouteMetadata {
    RouteMetadata {
        method: "GET".to_owned(),
        template: "/widget_maker/list".to_owned(),
    }
}

/// Invoke `platform-fixture:widget-maker/list@1.0.0` through a bound client.
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

/// Input for `platform-fixture:widget-maker/query@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetMakerQueryRequest {
    /// `text`, omittable
    pub cursor: Option<String>,
    /// `object`, omittable
    pub filter: Option<WidgetMakerQueryRequestFilter>,
    /// `int32`, omittable
    pub limit: Option<i32>,
    /// `object`, omittable
    pub sort: Option<WidgetMakerQueryRequestSort>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WidgetMakerQueryRequestFilter {
    /// `array`, omittable
    pub name: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WidgetMakerQueryRequestSort {
    /// `text`
    pub direction: String,
    /// `text`
    pub field: String,
}

/// Result of `platform-fixture:widget-maker/query@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetMakerQueryResult {
    /// `timestamptz`
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// `int64`
    pub edit_version: i64,
    /// `uuid`
    pub id: uuid::Uuid,
    /// `text`
    pub name: String,
}

/// Input descriptors for `platform-fixture:widget-maker/query@1.0.0`.
pub const WIDGET_MAKER_QUERY_INPUT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "cursor",
        type_name: "text",
        nullable: true,
        values: &[],
    },
    FieldDescriptor {
        path: "filter.name[]",
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
        path: "sort.direction",
        type_name: "text",
        nullable: false,
        values: &["ascending", "descending"],
    },
    FieldDescriptor {
        path: "sort.field",
        type_name: "text",
        nullable: false,
        values: &["created_at"],
    },
];

/// Result descriptors for `platform-fixture:widget-maker/query@1.0.0`.
pub const WIDGET_MAKER_QUERY_RESULT: &[FieldDescriptor] = &[
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
        path: "name",
        type_name: "text",
        nullable: false,
        values: &[],
    },
];

pub const WIDGET_MAKER_QUERY_INPUT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
                path: "filter.name[]",
                type_name: "array",
                nullable: false,
                values: &[],
            },
            required: false,
            minimum: None,
            maximum: None,
            children: &[wamn_client::descriptor::FieldSchema {
                field: FieldDescriptor {
                    path: "filter.name[]",
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
                    values: &["created_at"],
                },
                required: true,
                minimum: None,
                maximum: None,
                children: &[],
            },
        ],
    },
];

pub const WIDGET_MAKER_QUERY_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[
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
            path: "name",
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

pub const WIDGET_MAKER_QUERY_KIND: &str = "query";
pub const WIDGET_MAKER_QUERY_REQUIRES_COMPOSITION: bool = false;
pub const WIDGET_MAKER_QUERY_REPLAY: Option<&str> = None;
pub const WIDGET_MAKER_QUERY_RESPONSE_CONTRACT: Option<&str> = Some("{\"type\":\"array\"}");
pub const WIDGET_MAKER_QUERY_RESULT_OPAQUE: bool = false;
/// The grant a caller presents to invoke `platform-fixture:widget-maker/query@1.0.0`.
pub const WIDGET_MAKER_QUERY_GRANT: &str = "platform-fixture:widget-maker/query@1.0.0";

/// Typed refusals `platform-fixture:widget-maker/query@1.0.0` declares.
pub const WIDGET_MAKER_QUERY_ERRORS: &[&str] = &[
    "internal_error",
    "invalid_input",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `platform-fixture:widget-maker/query@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn query_route() -> RouteMetadata {
    RouteMetadata {
        method: "GET".to_owned(),
        template: "/widget_maker/query".to_owned(),
    }
}

/// Invoke `platform-fixture:widget-maker/query@1.0.0` through a bound client.
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
