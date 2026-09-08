// @generated from the client-contract IR; do not edit.
//!
//! `location` operations of package `wamn_receiving`.

use wamn_client::{ClientError, FieldDescriptor, RouteMetadata, WamnClient};

/// Every field the `location` model projects.
pub const LOCATION_FIELDS: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "location_code",
        type_name: "text",
        nullable: false,
        values: &[],
    },
];


/// Input for `wamn-receiving:location/list@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct LocationListRequest {
    /// `text`
    pub request_id: String,
}

/// Result of `wamn-receiving:location/list@1.0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct LocationListResult {
    /// `uuid`
    pub id: uuid::Uuid,
    /// `text`
    pub location_code: String,
}

/// Input descriptors for `wamn-receiving:location/list@1.0.0`.
pub const LOCATION_LIST_INPUT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "request_id",
        type_name: "text",
        nullable: false,
        values: &[],
    },
];

/// Result descriptors for `wamn-receiving:location/list@1.0.0`.
pub const LOCATION_LIST_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "location_code",
        type_name: "text",
        nullable: false,
        values: &[],
    },
];

/// The grant a caller presents to invoke `wamn-receiving:location/list@1.0.0`.
pub const LOCATION_LIST_GRANT: &str = "wamn-receiving:location/list@1.0.0";

/// Typed refusals `wamn-receiving:location/list@1.0.0` declares.
pub const LOCATION_LIST_ERRORS: &[&str] = &[
    "internal_error",
    "invalid_input",
    "permission_denied",
    "retry",
    "timeout",
];

/// Where the release publishes `wamn-receiving:location/list@1.0.0`.
///
/// Method and template only — the host and base URL are the client's
/// deployment config, not this release's facts.
#[must_use]
pub fn list_route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/location/list".to_owned(),
    }
}

/// Invoke `wamn-receiving:location/list@1.0.0` through a bound client.
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
