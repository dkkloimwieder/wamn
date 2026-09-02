use std::error::Error;
use std::fmt;

use sqlx_core::error::Error as SqlxError;
use wamn_postgres_sqlx::{WamnDatabaseError, WamnPgError};

/// Stable operation-level failure class.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessErrorKind {
    InvalidInput,
    NotFound,
    ConcurrencyConflict,
    Retry,
    Timeout,
    PermissionDenied,
    InternalError,
}

impl AccessErrorKind {
    /// Frozen operation-contract literal for this failure.
    pub const fn literal(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::NotFound => "not_found",
            Self::ConcurrencyConflict => "concurrency_conflict",
            Self::Retry => "retry",
            Self::Timeout => "timeout",
            Self::PermissionDenied => "permission_denied",
            Self::InternalError => "internal_error",
        }
    }
}

/// Contextual data-access failure translated once at the operation boundary.
#[derive(Debug)]
pub struct AccessError {
    kind: AccessErrorKind,
    context: Box<str>,
    field: Option<&'static str>,
    observed_row_version: Option<i64>,
}

impl AccessError {
    /// Stable class; callers do not match display text.
    pub const fn kind(&self) -> AccessErrorKind {
        self.kind
    }

    /// Stable contextual description for the node error boundary.
    pub fn context(&self) -> &str {
        &self.context
    }

    /// Input field owned by an invalid-input refusal.
    pub const fn field(&self) -> Option<&'static str> {
        self.field
    }

    /// Current revision returned by an optimistic-concurrency refusal.
    pub const fn observed_row_version(&self) -> Option<i64> {
        self.observed_row_version
    }

    pub(crate) fn invalid(context: impl Into<Box<str>>, field: &'static str) -> Self {
        Self {
            kind: AccessErrorKind::InvalidInput,
            context: context.into(),
            field: Some(field),
            observed_row_version: None,
        }
    }

    pub(crate) fn not_found(context: impl Into<Box<str>>) -> Self {
        Self::new(AccessErrorKind::NotFound, context)
    }

    pub(crate) fn concurrency_conflict(
        context: impl Into<Box<str>>,
        observed_row_version: i64,
    ) -> Self {
        Self {
            kind: AccessErrorKind::ConcurrencyConflict,
            context: context.into(),
            field: None,
            observed_row_version: Some(observed_row_version),
        }
    }

    pub(crate) fn internal(context: impl Into<Box<str>>) -> Self {
        Self::new(AccessErrorKind::InternalError, context)
    }

    pub(crate) fn from_sqlx(context: impl Into<Box<str>>, source: &SqlxError) -> Self {
        Self::new(classify(source), context)
    }

    fn new(kind: AccessErrorKind, context: impl Into<Box<str>>) -> Self {
        Self {
            kind,
            context: context.into(),
            field: None,
            observed_row_version: None,
        }
    }
}

impl fmt::Display for AccessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.kind.literal(), self.context)
    }
}

impl Error for AccessError {}

fn classify(error: &SqlxError) -> AccessErrorKind {
    let SqlxError::Database(database) = error else {
        return AccessErrorKind::InternalError;
    };
    let Some(database) = database.as_error().downcast_ref::<WamnDatabaseError>() else {
        return AccessErrorKind::InternalError;
    };
    classify_pg_error(database.pg_error())
}

fn classify_pg_error(error: &WamnPgError) -> AccessErrorKind {
    match error {
        WamnPgError::SerializationFailure | WamnPgError::ConnectionUnavailable => {
            AccessErrorKind::Retry
        }
        WamnPgError::StatementTimeout => AccessErrorKind::Timeout,
        WamnPgError::PermissionDenied => AccessErrorKind::PermissionDenied,
        WamnPgError::RowLimitExceeded(_)
        | WamnPgError::UniqueViolation(_)
        | WamnPgError::ForeignKeyViolation(_)
        | WamnPgError::CheckViolation(_)
        | WamnPgError::QueryError { .. } => AccessErrorKind::InternalError,
    }
}

#[cfg(test)]
mod tests {
    use super::{AccessErrorKind, classify_pg_error};
    use wamn_postgres_sqlx::WamnPgError;

    #[test]
    fn only_contractual_database_classes_cross_the_boundary() {
        for (source, expected) in [
            (WamnPgError::SerializationFailure, AccessErrorKind::Retry),
            (WamnPgError::ConnectionUnavailable, AccessErrorKind::Retry),
            (WamnPgError::StatementTimeout, AccessErrorKind::Timeout),
            (
                WamnPgError::PermissionDenied,
                AccessErrorKind::PermissionDenied,
            ),
            (
                WamnPgError::CheckViolation("undeclared".to_owned()),
                AccessErrorKind::InternalError,
            ),
        ] {
            assert_eq!(classify_pg_error(&source), expected);
        }
    }
}
