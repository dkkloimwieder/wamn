// @generated from the package manifest and migration IR; do not edit.

// The one refusal of the generated operations: the contract's literal and
// the detail members its error contract declares.

use std::fmt;

use wamn_postgres_statements::{StatementError, StatementErrorType};

/// One refusal, with the detail members its literal declares.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidInput {
        field: String,
        minimum: Option<String>,
        maximum: Option<String>,
        observed: Option<String>,
    },
    NotFound {
        field: String,
        id: String,
    },
    ConcurrencyConflict {
        expected_row_version: i64,
        observed_row_version: i64,
    },
    UniqueViolation {
        constraint: String,
    },
    ForeignKeyViolation {
        constraint: String,
    },
    CheckViolation {
        constraint: String,
    },
    ExclusionViolation {
        constraint: String,
    },
    Retry,
    Timeout,
    PermissionDenied {
        operation: String,
    },
    InternalError,
}

impl Error {
    /// The operation-contract literal.
    #[must_use]
    pub const fn literal(&self) -> &'static str {
        match self {
            Self::InvalidInput { .. } => "invalid_input",
            Self::NotFound { .. } => "not_found",
            Self::ConcurrencyConflict { .. } => "concurrency_conflict",
            Self::UniqueViolation { .. } => "unique_violation",
            Self::ForeignKeyViolation { .. } => "foreign_key_violation",
            Self::CheckViolation { .. } => "check_violation",
            Self::ExclusionViolation { .. } => "exclusion_violation",
            Self::Retry => "retry",
            Self::Timeout => "timeout",
            Self::PermissionDenied { .. } => "permission_denied",
            Self::InternalError => "internal_error",
        }
    }

    /// One declared detail member, spelled as the codec reads it.
    #[must_use]
    pub fn detail(&self, key: &str) -> Option<String> {
        match (self, key) {
            (Self::InvalidInput { field, .. } | Self::NotFound { field, .. }, "field") => {
                Some(field.clone())
            }
            (Self::NotFound { id, .. }, "id") => Some(id.clone()),
            (
                Self::ConcurrencyConflict {
                    expected_row_version,
                    ..
                },
                "expected_row_version",
            ) => Some(expected_row_version.to_string()),
            (
                Self::ConcurrencyConflict {
                    observed_row_version,
                    ..
                },
                "observed_row_version",
            ) => Some(observed_row_version.to_string()),
            (Self::InvalidInput { minimum, .. }, "minimum") => minimum.clone(),
            (Self::InvalidInput { maximum, .. }, "maximum") => maximum.clone(),
            (Self::InvalidInput { observed, .. }, "observed") => observed.clone(),
            (
                Self::UniqueViolation { constraint, .. }
                | Self::ForeignKeyViolation { constraint, .. }
                | Self::CheckViolation { constraint, .. }
                | Self::ExclusionViolation { constraint, .. },
                "constraint",
            ) => Some(constraint.clone()),
            (Self::PermissionDenied { operation, .. }, "operation") => Some(operation.clone()),
            _ => None,
        }
    }

    /// Refuse the value at `field`.
    #[must_use]
    pub fn invalid(field: &str) -> Self {
        Self::InvalidInput {
            field: field.to_owned(),
            minimum: None,
            maximum: None,
            observed: None,
        }
    }

    /// No row has this id.
    #[must_use]
    pub fn not_found(id: &wamn_postgres_statements::Uuid) -> Self {
        Self::NotFound {
            field: "id".to_owned(),
            id: id.0.clone(),
        }
    }

    /// The row carries another revision than the caller read.
    #[must_use]
    pub fn conflict(expected_row_version: i64, observed_row_version: i64) -> Self {
        Self::ConcurrencyConflict {
            expected_row_version,
            observed_row_version,
        }
    }

    /// A fault the contract does not name.
    #[must_use]
    pub fn internal() -> Self {
        Self::InternalError
    }

    /// The one translation of a statement failure. A constraint the
    /// operation does not name is an `internal_error`, as is every kind the
    /// contract does not name.
    #[must_use]
    pub fn from_statement(
        error: &StatementError,
        constraints: &Constraints,
        operation: &str,
    ) -> Self {
        Self::from_parts(error.kind(), error.constraint(), constraints, operation)
    }

    /// [`Error::from_statement`] over the kind and the constraint of a
    /// statement failure.
    #[must_use]
    pub fn from_parts(
        kind: StatementErrorType,
        constraint: Option<&str>,
        constraints: &Constraints,
        operation: &str,
    ) -> Self {
        let named = |names: &[&str], refusal: fn(String) -> Self| match constraint
            .filter(|name| names.contains(name))
        {
            Some(constraint) => refusal(constraint.to_owned()),
            None => Self::InternalError,
        };
        match kind {
            StatementErrorType::SerializationFailure
            | StatementErrorType::ConnectionUnavailable => Self::Retry,
            StatementErrorType::StatementTimeout => Self::Timeout,
            StatementErrorType::PermissionDenied => Self::PermissionDenied {
                operation: operation.to_owned(),
            },
            StatementErrorType::UniqueViolation => named(constraints.unique, |constraint| {
                Self::UniqueViolation { constraint }
            }),
            StatementErrorType::ForeignKeyViolation => {
                named(constraints.foreign_key, |constraint| {
                    Self::ForeignKeyViolation { constraint }
                })
            }
            StatementErrorType::CheckViolation => named(constraints.check, |constraint| {
                Self::CheckViolation { constraint }
            }),
            StatementErrorType::ExclusionViolation => named(constraints.exclusion, |constraint| {
                Self::ExclusionViolation { constraint }
            }),
            _ => Self::InternalError,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.literal())
    }
}

impl std::error::Error for Error {}

/// The constraint names one operation may report to its caller.
#[derive(Clone, Copy, Debug)]
pub struct Constraints {
    pub unique: &'static [&'static str],
    pub foreign_key: &'static [&'static str],
    pub check: &'static [&'static str],
    pub exclusion: &'static [&'static str],
}

impl Constraints {
    /// An operation that names no constraint.
    pub const NONE: Self = Self {
        unique: &[],
        foreign_key: &[],
        check: &[],
        exclusion: &[],
    };
}
