use std::error::Error;
use std::fmt;

use sqlx_core::error::Error as SqlxError;
use wamn_postgres_sqlx::{WamnDatabaseError, WamnPgError};

/// Exact named constraints an operation contract permits callers to observe.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AllowedConstraints {
    unique: &'static [&'static str],
    foreign_key: &'static [&'static str],
    check: &'static [&'static str],
}

impl AllowedConstraints {
    /// Read operations expose no named constraint violations.
    pub(crate) const NONE: Self = Self {
        unique: &[],
        foreign_key: &[],
        check: &[],
    };

    /// Build one operation policy from generator-owned constraint slices.
    pub(crate) const fn new(
        unique: &'static [&'static str],
        foreign_key: &'static [&'static str],
        check: &'static [&'static str],
    ) -> Self {
        Self {
            unique,
            foreign_key,
            check,
        }
    }

    fn permits_unique(self, name: &str) -> bool {
        self.unique.contains(&name)
    }

    fn permits_foreign_key(self, name: &str) -> bool {
        self.foreign_key.contains(&name)
    }

    fn permits_check(self, name: &str) -> bool {
        self.check.contains(&name)
    }
}

/// Stable operation-level error class returned by Receiving accessors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessErrorKind {
    InvalidInput,
    NotFound,
    ConcurrencyConflict,
    UniqueViolation,
    ForeignKeyViolation,
    CheckViolation,
    Retry,
    Timeout,
    PermissionDenied,
    InternalError,
}

impl AccessErrorKind {
    /// Frozen operation-contract literal for this error class.
    pub const fn literal(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::NotFound => "not_found",
            Self::ConcurrencyConflict => "concurrency_conflict",
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

/// Contextual Receiving failure translated once at the operation boundary.
#[derive(Debug)]
pub struct AccessError {
    kind: AccessErrorKind,
    context: Box<str>,
    constraint: Option<Box<str>>,
}

impl AccessError {
    /// Stable class; callers must not match display text.
    pub const fn kind(&self) -> AccessErrorKind {
        self.kind
    }

    /// Named PostgreSQL constraint for typed violation cases.
    pub fn constraint(&self) -> Option<&str> {
        self.constraint.as_deref()
    }

    pub(crate) fn invalid(context: impl Into<Box<str>>) -> Self {
        Self::new(AccessErrorKind::InvalidInput, context)
    }

    pub(crate) fn not_found(context: impl Into<Box<str>>) -> Self {
        Self::new(AccessErrorKind::NotFound, context)
    }

    pub(crate) fn concurrency_conflict(context: impl Into<Box<str>>) -> Self {
        Self::new(AccessErrorKind::ConcurrencyConflict, context)
    }

    pub(crate) fn internal(context: impl Into<Box<str>>) -> Self {
        Self::new(AccessErrorKind::InternalError, context)
    }

    pub(crate) fn from_sqlx(
        context: impl Into<Box<str>>,
        source: &SqlxError,
        allowed_constraints: AllowedConstraints,
    ) -> Self {
        let (kind, constraint) = classify(source, allowed_constraints);
        Self {
            kind,
            context: context.into(),
            constraint,
        }
    }

    fn new(kind: AccessErrorKind, context: impl Into<Box<str>>) -> Self {
        Self {
            kind,
            context: context.into(),
            constraint: None,
        }
    }
}

impl fmt::Display for AccessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.kind.literal(), self.context)
    }
}

impl Error for AccessError {}

fn classify(
    error: &SqlxError,
    allowed_constraints: AllowedConstraints,
) -> (AccessErrorKind, Option<Box<str>>) {
    let SqlxError::Database(database) = error else {
        return (AccessErrorKind::InternalError, None);
    };
    let Some(database) = database.as_error().downcast_ref::<WamnDatabaseError>() else {
        return (AccessErrorKind::InternalError, None);
    };
    classify_pg_error(database.pg_error(), allowed_constraints)
}

fn classify_pg_error(
    error: &WamnPgError,
    allowed_constraints: AllowedConstraints,
) -> (AccessErrorKind, Option<Box<str>>) {
    match error {
        WamnPgError::SerializationFailure | WamnPgError::ConnectionUnavailable => {
            (AccessErrorKind::Retry, None)
        }
        WamnPgError::StatementTimeout => (AccessErrorKind::Timeout, None),
        WamnPgError::UniqueViolation(constraint)
            if allowed_constraints.permits_unique(constraint) =>
        {
            named_violation(AccessErrorKind::UniqueViolation, constraint)
        }
        WamnPgError::ForeignKeyViolation(constraint)
            if allowed_constraints.permits_foreign_key(constraint) =>
        {
            named_violation(AccessErrorKind::ForeignKeyViolation, constraint)
        }
        WamnPgError::CheckViolation(constraint)
            if allowed_constraints.permits_check(constraint) =>
        {
            named_violation(AccessErrorKind::CheckViolation, constraint)
        }
        WamnPgError::UniqueViolation(_)
        | WamnPgError::ForeignKeyViolation(_)
        | WamnPgError::CheckViolation(_) => (AccessErrorKind::InternalError, None),
        WamnPgError::PermissionDenied => (AccessErrorKind::PermissionDenied, None),
        WamnPgError::RowLimitExceeded(_) | WamnPgError::QueryError { .. } => {
            (AccessErrorKind::InternalError, None)
        }
    }
}

fn named_violation(kind: AccessErrorKind, constraint: &str) -> (AccessErrorKind, Option<Box<str>>) {
    (kind, Some(constraint.into()))
}

#[cfg(test)]
mod tests {
    use super::{AccessErrorKind, AllowedConstraints, classify_pg_error};
    use wamn_postgres_sqlx::WamnPgError;

    const UPDATE_CONSTRAINTS: AllowedConstraints = AllowedConstraints {
        unique: &["allowed_unique"],
        foreign_key: &["allowed_foreign_key"],
        check: &["allowed_check"],
    };

    #[test]
    fn reads_hide_all_named_constraint_violations() {
        let errors = [
            WamnPgError::UniqueViolation("hidden_unique".to_owned()),
            WamnPgError::ForeignKeyViolation("hidden_foreign_key".to_owned()),
            WamnPgError::CheckViolation("hidden_check".to_owned()),
        ];

        for error in errors {
            assert_eq!(
                classify_pg_error(&error, AllowedConstraints::NONE),
                (AccessErrorKind::InternalError, None)
            );
        }
    }

    #[test]
    fn operation_contract_exposes_only_exact_names_of_the_expected_kind() {
        let accepted = [
            (
                WamnPgError::UniqueViolation("allowed_unique".to_owned()),
                AccessErrorKind::UniqueViolation,
            ),
            (
                WamnPgError::ForeignKeyViolation("allowed_foreign_key".to_owned()),
                AccessErrorKind::ForeignKeyViolation,
            ),
            (
                WamnPgError::CheckViolation("allowed_check".to_owned()),
                AccessErrorKind::CheckViolation,
            ),
        ];

        for (error, expected_kind) in accepted {
            let expected_constraint = match &error {
                WamnPgError::UniqueViolation(name)
                | WamnPgError::ForeignKeyViolation(name)
                | WamnPgError::CheckViolation(name) => name.as_str(),
                _ => unreachable!("fixture contains only named violations"),
            };
            let (kind, constraint) = classify_pg_error(&error, UPDATE_CONSTRAINTS);
            assert_eq!(kind, expected_kind);
            assert_eq!(constraint.as_deref(), Some(expected_constraint));
        }

        let rejected = [
            WamnPgError::UniqueViolation("unknown_unique".to_owned()),
            WamnPgError::ForeignKeyViolation("unknown_foreign_key".to_owned()),
            WamnPgError::CheckViolation("unknown_check".to_owned()),
            WamnPgError::CheckViolation("allowed_unique".to_owned()),
        ];

        for error in rejected {
            assert_eq!(
                classify_pg_error(&error, UPDATE_CONSTRAINTS),
                (AccessErrorKind::InternalError, None)
            );
        }
    }

    #[test]
    fn non_constraint_transport_meanings_are_preserved() {
        let cases = [
            (WamnPgError::SerializationFailure, AccessErrorKind::Retry),
            (WamnPgError::ConnectionUnavailable, AccessErrorKind::Retry),
            (WamnPgError::StatementTimeout, AccessErrorKind::Timeout),
            (
                WamnPgError::PermissionDenied,
                AccessErrorKind::PermissionDenied,
            ),
            (
                WamnPgError::RowLimitExceeded(100),
                AccessErrorKind::InternalError,
            ),
            (
                WamnPgError::QueryError {
                    sqlstate: "XX000".to_owned(),
                    message: "opaque".to_owned(),
                },
                AccessErrorKind::InternalError,
            ),
        ];

        for (error, expected_kind) in cases {
            assert_eq!(
                classify_pg_error(&error, AllowedConstraints::NONE),
                (expected_kind, None)
            );
        }
    }
}
