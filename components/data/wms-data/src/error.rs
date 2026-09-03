//! The refusals `inventory.move` declares, and the one translation into them.
//!
//! Deliberately narrower than Receiving's: this crate implements ONE command,
//! so it carries the literals that command's contract names and no others. A
//! wider taxonomy copied across would let a caller observe a class the
//! contract never promised.

use std::error::Error;
use std::fmt;

use wamn_postgres_statements::{StatementError, StatementErrorKind};

/// Stable operation-contract literal for one refusal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessErrorKind {
    /// The body was not what the input contract admits.
    InvalidInput,
    /// The named pallet does not exist.
    PalletNotFound,
    /// The destination location does not exist.
    LocationNotFound,
    /// The caller wrote against a revision the row no longer carries.
    ConcurrencyConflict,
    /// A second delivery under one key carried a DIFFERENT command body.
    IdempotencyConflict,
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
            Self::PalletNotFound => "pallet_not_found",
            Self::LocationNotFound => "location_not_found",
            Self::ConcurrencyConflict => "concurrency_conflict",
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

    /// A not-found refusal, which names the field AND the id looked for.
    #[must_use]
    pub fn missing(kind: AccessErrorKind, field: &str, id: &str) -> Self {
        Self::new(kind, serde_json::json!({ "field": field, "id": id }))
    }

    /// A stale write, carrying BOTH revisions: the expected one alone cannot
    /// tell a caller whether to retry or to look at what moved underneath.
    #[must_use]
    pub fn conflict(expected: i64, observed: i64) -> Self {
        Self::new(
            AccessErrorKind::ConcurrencyConflict,
            serde_json::json!({
                "expected_row_version": expected,
                "observed_row_version": observed,
            }),
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
/// the contract never named — an unknown statement or a contract mismatch is a
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A conflict carries both revisions — the assertion the whole
    /// optimistic-concurrency contract rests on.
    #[test]
    fn a_conflict_carries_both_revisions() {
        let error = AccessError::conflict(4, 7);
        assert_eq!(error.kind().literal(), "concurrency_conflict");
        assert_eq!(error.detail()["expected_row_version"], 4);
        assert_eq!(error.detail()["observed_row_version"], 7);
    }

    /// Every literal this crate can produce is one `inventory.move` declares.
    #[test]
    fn every_literal_is_declared_by_the_operation_contract() {
        let contract: serde_json::Value = serde_json::from_slice(
            &std::fs::read(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../../packages/wms/generated/contracts/inventory/move.errors.json"),
            )
            .expect("the move errors contract is generated"),
        )
        .expect("parses");
        let declared: Vec<&str> = contract["cases"]
            .as_array()
            .expect("cases")
            .iter()
            .map(|case| case["literal"].as_str().expect("literal"))
            .collect();

        for kind in [
            AccessErrorKind::InvalidInput,
            AccessErrorKind::PalletNotFound,
            AccessErrorKind::LocationNotFound,
            AccessErrorKind::ConcurrencyConflict,
            AccessErrorKind::IdempotencyConflict,
            AccessErrorKind::Retry,
            AccessErrorKind::Timeout,
            AccessErrorKind::PermissionDenied,
            AccessErrorKind::InternalError,
        ] {
            assert!(
                declared.contains(&kind.literal()),
                "{} is not declared by the contract: {declared:?}",
                kind.literal()
            );
        }
    }
}
