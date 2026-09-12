//! The refusals the dock-appointment operations declare, and the one
//! translation into them.
//!
//! Every literal here is one some operation's contract names. Each operation
//! module lists what it can refuse with, and the test below holds every list
//! to that operation's generated errors contract, so a caller never observes a
//! class the contract it read did not promise.

use std::error::Error;
use std::fmt;

use wamn_postgres_statements::{StatementError, StatementErrorKind};

/// Stable operation-contract literal for one refusal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessErrorKind {
    /// The body was not what the input contract admits.
    InvalidInput,
    /// The named appointment does not exist.
    NotFound,
    /// The named carrier does not exist.
    CarrierNotFound,
    /// The named dock does not exist.
    DockNotFound,
    /// The requested slot overlaps an appointment already on that dock.
    SlotUnavailable,
    /// The appointment already moved past `scheduled`, and status only moves
    /// forward.
    AppointmentNotScheduled,
    /// A second delivery under one key carried a DIFFERENT command body.
    IdempotencyConflict,
    /// Transient; the caller can send the same command again.
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
            Self::CarrierNotFound => "carrier_not_found",
            Self::DockNotFound => "dock_not_found",
            Self::SlotUnavailable => "slot_unavailable",
            Self::AppointmentNotScheduled => "appointment_not_scheduled",
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

    /// A refusal that names the field AND the id it looked for.
    #[must_use]
    pub fn missing(kind: AccessErrorKind, field: &str, id: &str) -> Self {
        Self::new(kind, serde_json::json!({ "field": field, "id": id }))
    }

    /// Transient failure the caller can repeat under the same key.
    #[must_use]
    pub fn retry() -> Self {
        Self::new(AccessErrorKind::Retry, serde_json::json!({}))
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
/// the contract never named: an unknown statement or a contract mismatch is a
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

    #[test]
    fn a_not_found_refusal_names_the_field_and_the_id() {
        let error = AccessError::missing(AccessErrorKind::NotFound, "value.appointment_id", "x");
        assert_eq!(error.kind().literal(), "not_found");
        assert_eq!(error.detail()["field"], "value.appointment_id");
        assert_eq!(error.detail()["id"], "x");
    }

    /// Every literal an operation can produce is one ITS contract declares.
    /// The lists are the modules' own; a refusal added to a module without
    /// being added to the manifest fails here, before a caller sees it.
    #[test]
    fn every_operation_refuses_only_what_its_contract_declares() {
        let contracts: [(&str, &[AccessErrorKind]); 5] = [
            ("carrier/create", crate::carrier_create::REFUSALS),
            ("dock/create", crate::dock_create::REFUSALS),
            ("appointment/book", crate::appointment_book::REFUSALS),
            (
                "appointment/check_in",
                crate::appointment_check_in::REFUSALS,
            ),
            ("appointment/query", crate::appointment_query::REFUSALS),
        ];
        for (operation, refusals) in contracts {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../packages/dock/generated/contracts")
                .join(format!("{operation}.errors.json"));
            let contract: serde_json::Value = serde_json::from_slice(
                &std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display())),
            )
            .expect("parses");
            let declared: Vec<&str> = contract["cases"]
                .as_array()
                .expect("cases")
                .iter()
                .map(|case| case["literal"].as_str().expect("literal"))
                .collect();
            assert!(
                !refusals.is_empty(),
                "{operation} lists what it refuses with"
            );
            for kind in refusals {
                assert!(
                    declared.contains(&kind.literal()),
                    "{operation}: {} is not declared by the contract: {declared:?}",
                    kind.literal()
                );
            }
        }
    }
}
