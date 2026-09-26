//! Edge sample operation failures and their translation from statement errors.

use std::error::Error;
use std::fmt;

use wamn_postgres_statements::{StatementError, StatementErrorKind};

/// Stable operation-contract literal for one refusal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessErrorKind {
    /// The body was not what the input contract admits.
    InvalidInput,
    /// A read named a row that does not exist.
    NotFound,
    /// A second delivery under one key carried a different command body.
    IdempotencyConflict,
    /// Transient. The caller can send the same command again.
    Retry,
    /// The statement exceeded its time budget.
    Timeout,
    /// The caller may not invoke this operation.
    PermissionDenied,
    /// Anything the contract does not name.
    InternalError,
}

impl AccessErrorKind {
    /// Frozen operation-contract literal.
    #[must_use]
    pub const fn literal(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::NotFound => "not_found",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::Retry => "retry",
            Self::Timeout => "timeout",
            Self::PermissionDenied => "permission_denied",
            Self::InternalError => "internal_error",
        }
    }
}

/// One refusal, with the structured detail its literal declares.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccessError {
    kind: AccessErrorKind,
    detail: serde_json::Value,
}

impl AccessError {
    /// A refusal carrying the detail members its literal requires.
    #[must_use]
    pub fn new(kind: AccessErrorKind, detail: serde_json::Value) -> Self {
        Self { kind, detail }
    }

    /// A refusal naming the offending field.
    #[must_use]
    pub fn field(kind: AccessErrorKind, field: &str) -> Self {
        Self::new(kind, serde_json::json!({ "field": field }))
    }

    /// A not-found refusal, which names the field and the id looked for.
    #[must_use]
    pub fn missing(kind: AccessErrorKind, field: &str, id: &str) -> Self {
        Self::new(kind, serde_json::json!({ "field": field, "id": id }))
    }

    /// What went wrong.
    #[must_use]
    pub const fn kind(&self) -> AccessErrorKind {
        self.kind
    }

    /// The declared detail members.
    #[must_use]
    pub const fn detail(&self) -> &serde_json::Value {
        &self.detail
    }
}

impl fmt::Display for AccessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.kind.literal())
    }
}

impl Error for AccessError {}

/// The one translation of a statement failure into the contract vocabulary.
///
/// Every unmapped kind lands on `internal_error`, because an unknown statement
/// or a contract mismatch is a deployment fault that a caller cannot act on.
#[must_use]
pub fn from_statement(error: &StatementError) -> AccessError {
    let kind = match error.kind() {
        StatementErrorKind::SerializationFailure | StatementErrorKind::ConnectionUnavailable => {
            AccessErrorKind::Retry
        }
        StatementErrorKind::StatementTimeout => AccessErrorKind::Timeout,
        StatementErrorKind::PermissionDenied => AccessErrorKind::PermissionDenied,
        _ => AccessErrorKind::InternalError,
    };
    AccessError::new(kind, serde_json::json!({}))
}
