//! Edge sample operation failures and their translation from statement errors.

use std::error::Error;
use std::fmt;

use wamn_postgres_statements::{StatementError, StatementErrorType};

/// Stable operation-contract literal for one refusal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessErrorType {
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

impl AccessErrorType {
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
    type_: AccessErrorType,
    detail: serde_json::Value,
}

impl AccessError {
    /// A refusal carrying the detail members its literal requires.
    #[must_use]
    pub fn new(kind: AccessErrorType, detail: serde_json::Value) -> Self {
        Self {
            type_: kind,
            detail,
        }
    }

    /// A refusal naming the offending field.
    #[must_use]
    pub fn field(kind: AccessErrorType, field: &str) -> Self {
        Self::new(kind, serde_json::json!({ "field": field }))
    }

    /// What went wrong.
    #[must_use]
    pub const fn kind(&self) -> AccessErrorType {
        self.type_
    }

    /// The declared detail members.
    #[must_use]
    pub const fn detail(&self) -> &serde_json::Value {
        &self.detail
    }
}

impl fmt::Display for AccessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.type_.literal())
    }
}

impl Error for AccessError {}

/// The one translation of a statement failure into the contract vocabulary.
///
/// Every unmapped kind lands on `internal_error`, because an unknown statement
/// or a contract mismatch is a deployment fault that a caller cannot act on.
#[must_use]
pub fn from_statement(error: &StatementError) -> AccessError {
    let kind = match error.type_() {
        StatementErrorType::SerializationFailure | StatementErrorType::ConnectionUnavailable => {
            AccessErrorType::Retry
        }
        StatementErrorType::StatementTimeout => AccessErrorType::Timeout,
        StatementErrorType::PermissionDenied => AccessErrorType::PermissionDenied,
        _ => AccessErrorType::InternalError,
    };
    AccessError::new(kind, serde_json::json!({}))
}
