//! Shared failures and statement translation for generated data functions.
use wamn_postgres_statements::{StatementError, StatementErrorType};

/// A declared data failure. Generated exports translate it to their WIT error.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    CursorExpired,
}
impl Error {
    /// The public refusal literal.
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
            Self::CursorExpired => "cursor_expired",
        }
    }
    /// One detail member.
    pub fn detail(&self, key: &str) -> Option<String> {
        match (self, key) {
            (Self::InvalidInput { field, .. }, "field") => Some(field.to_string()),
            (Self::InvalidInput { minimum, .. }, "minimum") => minimum.clone(),
            (Self::InvalidInput { maximum, .. }, "maximum") => maximum.clone(),
            (Self::InvalidInput { observed, .. }, "observed") => observed.clone(),
            (Self::NotFound { field, .. }, "field") => Some(field.to_string()),
            (Self::NotFound { id, .. }, "id") => Some(id.to_string()),
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
            (Self::UniqueViolation { constraint, .. }, "constraint") => {
                Some(constraint.to_string())
            }
            (Self::ForeignKeyViolation { constraint, .. }, "constraint") => {
                Some(constraint.to_string())
            }
            (Self::CheckViolation { constraint, .. }, "constraint") => Some(constraint.to_string()),
            (Self::ExclusionViolation { constraint, .. }, "constraint") => {
                Some(constraint.to_string())
            }
            (Self::PermissionDenied { operation, .. }, "operation") => Some(operation.to_string()),
            _ => None,
        }
    }
    /// Refuse an input value.
    pub fn invalid(field: &str) -> Self {
        Self::InvalidInput {
            field: field.to_owned(),
            minimum: None,
            maximum: None,
            observed: None,
        }
    }
    /// No row carries this key.
    pub fn not_found(id: &wamn_postgres_statements::Uuid) -> Self {
        Self::NotFound {
            field: "id".to_owned(),
            id: id.0.clone(),
        }
    }
    /// The feed cursor expired.
    pub fn cursor_expired() -> Self {
        Self::CursorExpired
    }
    /// The record revision changed.
    pub fn conflict(expected_row_version: i64, observed_row_version: i64) -> Self {
        Self::ConcurrencyConflict {
            expected_row_version,
            observed_row_version,
        }
    }
    /// An undeclared failure.
    pub fn internal() -> Self {
        Self::InternalError
    }
    /// Translate the database failure once.
    pub fn from_statement(
        error: &StatementError,
        constraints: &Constraints,
        operation: &str,
    ) -> Self {
        Self::from_parts(error.type_(), error.constraint(), constraints, operation)
    }
    /// Translate a statement failure with its declared constraint names.
    pub fn from_parts(
        type_: StatementErrorType,
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
        match type_ {
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
impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.literal())
    }
}
impl std::error::Error for Error {}
/// Constraint names that one operation can report.
#[derive(Debug, Clone, Copy)]
pub struct Constraints {
    pub unique: &'static [&'static str],
    pub foreign_key: &'static [&'static str],
    pub check: &'static [&'static str],
    pub exclusion: &'static [&'static str],
}
impl Constraints {
    /// No declared constraints.
    pub const NONE: Self = Self {
        unique: &[],
        foreign_key: &[],
        check: &[],
        exclusion: &[],
    };
}
