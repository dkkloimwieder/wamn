//! WMS operation failures and their translation from statement errors.

use std::error::Error;
use std::fmt;

use wamn_postgres_statements::{StatementError, StatementErrorKind};

/// Stable operation-contract literal for one refusal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessErrorKind {
    /// The body was not what the input contract admits.
    InvalidInput,
    /// A model read named a row that does not exist.
    NotFound,
    /// The named pallet does not exist, or is consumed and so not live stock.
    PalletNotFound,
    /// The destination location does not exist.
    LocationNotFound,
    /// The pallet holds no quantity row for that product and status.
    QuantityNotFound,
    /// The quantity row cannot spare what was asked and still hold stock.
    InsufficientQuantity,
    /// The caller wrote against a revision the row no longer carries.
    ConcurrencyConflict,
    /// A second delivery under one key carried a DIFFERENT command body.
    IdempotencyConflict,
    /// A generated write broke a unique constraint its contract names.
    UniqueViolation,
    /// A generated write named a row that does not exist.
    ForeignKeyViolation,
    /// A generated write broke a check constraint its contract names.
    CheckViolation,
    /// Transient; the caller may send the same command again.
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
            Self::PalletNotFound => "pallet_not_found",
            Self::LocationNotFound => "location_not_found",
            Self::QuantityNotFound => "quantity_not_found",
            Self::InsufficientQuantity => "insufficient_quantity",
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

    /// An out-of-range refusal, carrying the bounds and what was sent.
    #[must_use]
    pub fn range(field: &str, minimum: i64, maximum: i64, observed: i64) -> Self {
        Self::new(
            AccessErrorKind::InvalidInput,
            serde_json::json!({
                "field": field,
                "minimum": minimum,
                "maximum": maximum,
                "observed": observed,
            }),
        )
    }

    /// A not-found refusal, which names the field AND the id looked for.
    #[must_use]
    pub fn missing(kind: AccessErrorKind, field: &str, id: &str) -> Self {
        Self::new(kind, serde_json::json!({ "field": field, "id": id }))
    }

    /// A stale write, carrying BOTH revisions: the expected one alone cannot
    /// tell a caller whether to retry or to look at what moved underneath.
    #[must_use]
    pub fn conflict(expected: i32, observed: i32) -> Self {
        Self::new(
            AccessErrorKind::ConcurrencyConflict,
            serde_json::json!({
                "expected_row_version": expected,
                "observed_row_version": observed,
            }),
        )
    }

    /// A split asking for more than the row can spare, carrying what it holds
    /// so the caller can ask again for less rather than guess.
    #[must_use]
    pub fn insufficient(field: &str, observed: &str) -> Self {
        Self::new(
            AccessErrorKind::InsufficientQuantity,
            serde_json::json!({ "field": field, "observed": observed }),
        )
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

/// The ONE translation of a statement failure into the contract vocabulary.
///
/// Every unmapped kind lands on `internal_error` rather than leaking a class
/// the contract never named -- an unknown statement or a contract mismatch is a
/// deployment fault, not something a caller can act on.
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

/// The translation for a generated write, which may also name a constraint.
///
/// A violation of a constraint that the contract lists carries its name. Any
/// other violation falls to [`from_statement`] and so to `internal_error`.
#[must_use]
pub(crate) fn from_write(
    error: &StatementError,
    unique: &[&str],
    foreign_key: &[&str],
    check: &[&str],
) -> AccessError {
    let (kind, listed) = match error.kind() {
        StatementErrorKind::UniqueViolation => (AccessErrorKind::UniqueViolation, unique),
        StatementErrorKind::ForeignKeyViolation => {
            (AccessErrorKind::ForeignKeyViolation, foreign_key)
        }
        StatementErrorKind::CheckViolation => (AccessErrorKind::CheckViolation, check),
        _ => return from_statement(error),
    };
    match error.constraint() {
        Some(constraint) if listed.contains(&constraint) => {
            AccessError::new(kind, serde_json::json!({ "constraint": constraint }))
        }
        _ => from_statement(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A conflict carries both revisions -- the assertion the whole
    /// optimistic-concurrency contract rests on.
    #[test]
    fn a_conflict_carries_both_revisions() {
        let error = AccessError::conflict(4, 7);
        assert_eq!(error.kind().literal(), "concurrency_conflict");
        assert_eq!(error.detail()["expected_row_version"], 4);
        assert_eq!(error.detail()["observed_row_version"], 7);
    }
}
