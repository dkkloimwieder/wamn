//! Fixture operation failures and their translation from statement errors.

use std::error::Error;
use std::fmt;

use serde_json::{Value, json};
use wamn_postgres_statements::{StatementError, StatementErrorKind};

/// Stable operation-contract literal for one refusal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessErrorKind {
    /// The body was not what the input contract admits.
    InvalidInput,
    /// The named row does not exist.
    NotFound,
    /// The caller wrote against a revision the row no longer carries.
    ConcurrencyConflict,
    /// A second delivery under one key carried a different command body.
    IdempotencyConflict,
    /// The write broke a unique constraint the operation names.
    UniqueViolation,
    /// The write broke a foreign key the operation names.
    ForeignKeyViolation,
    /// The write broke a check constraint the operation names.
    CheckViolation,
    /// Transient. The caller may send the same request again.
    Retry,
    /// The statement exceeded its time budget.
    Timeout,
    /// The caller may not invoke this operation.
    PermissionDenied,
    /// Anything the contract does not name. Deliberately opaque.
    InternalError,
}

impl AccessErrorKind {
    /// Frozen operation-contract literal.
    #[must_use]
    pub const fn literal(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::NotFound => "not_found",
            Self::ConcurrencyConflict => "concurrency_conflict",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::UniqueViolation => "unique_violation",
            Self::ForeignKeyViolation => "foreign_key_violation",
            Self::CheckViolation => "check_violation",
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
    detail: Value,
}

impl AccessError {
    /// What went wrong.
    #[must_use]
    pub const fn kind(&self) -> AccessErrorKind {
        self.kind
    }

    /// The declared detail members.
    #[must_use]
    pub const fn detail(&self) -> &Value {
        &self.detail
    }

    pub(crate) fn field(kind: AccessErrorKind, field: &str) -> Self {
        Self::new(kind, json!({ "field": field }))
    }

    pub(crate) fn range(field: &str, minimum: i64, maximum: i64, observed: i64) -> Self {
        Self::new(
            AccessErrorKind::InvalidInput,
            json!({
                "field": field,
                "minimum": minimum,
                "maximum": maximum,
                "observed": observed,
            }),
        )
    }

    pub(crate) fn missing(id: &str) -> Self {
        Self::new(
            AccessErrorKind::NotFound,
            json!({ "field": "id", "id": id }),
        )
    }

    pub(crate) fn conflict(expected: i64, observed: i64) -> Self {
        Self::new(
            AccessErrorKind::ConcurrencyConflict,
            json!({
                "expected_row_version": expected,
                "observed_row_version": observed,
            }),
        )
    }

    pub(crate) fn internal() -> Self {
        Self::new(AccessErrorKind::InternalError, json!({}))
    }

    /// The one translation of a statement failure into the contract vocabulary.
    ///
    /// A constraint the operation does not name lands on `internal_error`, as
    /// does every unmapped kind.
    pub(crate) fn from_statement(error: &StatementError, constraints: Constraints) -> Self {
        let violation =
            |kind, names: &[&str]| match error.constraint().filter(|name| names.contains(name)) {
                Some(constraint) => Self::new(kind, json!({ "constraint": constraint })),
                None => Self::internal(),
            };
        match error.kind() {
            StatementErrorKind::SerializationFailure
            | StatementErrorKind::ConnectionUnavailable => {
                Self::new(AccessErrorKind::Retry, json!({}))
            }
            StatementErrorKind::StatementTimeout => Self::new(AccessErrorKind::Timeout, json!({})),
            StatementErrorKind::PermissionDenied => {
                Self::new(AccessErrorKind::PermissionDenied, json!({}))
            }
            StatementErrorKind::UniqueViolation => {
                violation(AccessErrorKind::UniqueViolation, constraints.unique)
            }
            StatementErrorKind::ForeignKeyViolation => violation(
                AccessErrorKind::ForeignKeyViolation,
                constraints.foreign_key,
            ),
            StatementErrorKind::CheckViolation => {
                violation(AccessErrorKind::CheckViolation, constraints.check)
            }
            _ => Self::internal(),
        }
    }

    fn new(kind: AccessErrorKind, detail: Value) -> Self {
        Self { kind, detail }
    }
}

impl fmt::Display for AccessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.kind.literal())
    }
}

impl Error for AccessError {}

/// The constraint names one operation may report to its caller.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Constraints {
    pub(crate) unique: &'static [&'static str],
    pub(crate) foreign_key: &'static [&'static str],
    pub(crate) check: &'static [&'static str],
}

impl Constraints {
    pub(crate) const NONE: Self = Self {
        unique: &[],
        foreign_key: &[],
        check: &[],
    };
}
