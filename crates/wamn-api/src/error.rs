//! Request-compilation errors, each carrying an HTTP status + a stable machine
//! code so the serving component can render a uniform JSON error body.
//!
//! Every one of these is raised **before** any SQL is built — they are the
//! allowlist rejections (unknown entity/field/relation/operator) and the value
//! validation failures (bad uuid, non-exact decimal, enum not a variant, …).
//! An unknown identifier can never reach the database.

use std::fmt;

use serde_json::{Value, json};

/// A request that the gateway refuses to compile. Maps to a 4xx status.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ApiError {
    /// The `{entity}` path segment does not name a catalog entity.
    UnknownEntity(String),
    /// A filter/sort/body key does not name a field on the entity.
    UnknownField { entity: String, field: String },
    /// `?expand=` names a relation not reachable from the entity.
    UnknownRelation { entity: String, relation: String },
    /// A value failed type/format/range validation for its field.
    InvalidValue { field: String, message: String },
    /// The request was malformed (bad path, unparseable limit, empty update, …).
    InvalidRequest(String),
    /// A write with no body.
    PayloadRequired,
    /// The method is not allowed for this route shape.
    MethodNotAllowed,
    /// The path did not match `<base>/{entity}[/{id}]`.
    NotFound,
}

impl ApiError {
    /// The HTTP status this error maps to.
    pub fn status(&self) -> u16 {
        match self {
            ApiError::NotFound => 404,
            ApiError::MethodNotAllowed => 405,
            _ => 400,
        }
    }

    /// A stable machine-readable code for the error body.
    pub fn code(&self) -> &'static str {
        match self {
            ApiError::UnknownEntity(_) => "unknown-entity",
            ApiError::UnknownField { .. } => "unknown-field",
            ApiError::UnknownRelation { .. } => "unknown-relation",
            ApiError::InvalidValue { .. } => "invalid-value",
            ApiError::InvalidRequest(_) => "invalid-request",
            ApiError::PayloadRequired => "payload-required",
            ApiError::MethodNotAllowed => "method-not-allowed",
            ApiError::NotFound => "not-found",
        }
    }

    /// A human-readable message for the error body.
    pub fn message(&self) -> String {
        match self {
            ApiError::UnknownEntity(e) => format!("no such entity: {e}"),
            ApiError::UnknownField { entity, field } => {
                format!("no such field on {entity}: {field}")
            }
            ApiError::UnknownRelation { entity, relation } => {
                format!("no such relation on {entity}: {relation}")
            }
            ApiError::InvalidValue { field, message } => {
                format!("invalid value for {field}: {message}")
            }
            ApiError::InvalidRequest(m) => m.clone(),
            ApiError::PayloadRequired => "a request body is required".into(),
            ApiError::MethodNotAllowed => "method not allowed for this route".into(),
            ApiError::NotFound => "not found".into(),
        }
    }

    /// The JSON error body: `{"error": {"code": ..., "message": ...}}`.
    pub fn to_json(&self) -> Value {
        json!({ "error": { "code": self.code(), "message": self.message() } })
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} [{}] {}", self.status(), self.code(), self.message())
    }
}

impl std::error::Error for ApiError {}
