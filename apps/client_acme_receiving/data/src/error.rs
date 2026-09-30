use std::error::Error;
use std::fmt;

use wamn_postgres_statements::{StatementError, StatementErrorType};

/// Exact exclusion names an operation contract permits callers to observe.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AllowedConstraints {
    pub(crate) exclusion: &'static [&'static str],
}

impl AllowedConstraints {
    pub(crate) const NONE: Self = Self { exclusion: &[] };

    fn permits_exclusion(self, name: &str) -> bool {
        self.exclusion.contains(&name)
    }
}

/// Stable operation-level failure class.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessErrorType {
    InvalidInput,
    NotFound,
    ConcurrencyConflict,
    ExclusionViolation,
    Retry,
    Timeout,
    PermissionDenied,
    InternalError,
}

impl AccessErrorType {
    /// Frozen operation-contract literal for this failure.
    pub const fn literal(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::NotFound => "not_found",
            Self::ConcurrencyConflict => "concurrency_conflict",
            Self::ExclusionViolation => "exclusion_violation",
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
    type_: AccessErrorType,
    context: Box<str>,
    constraint: Option<Box<str>>,
    field: Option<&'static str>,
    observed_row_version: Option<i32>,
}

impl AccessError {
    /// Stable class; callers do not match display text.
    pub const fn kind(&self) -> AccessErrorType {
        self.type_
    }

    /// Stable contextual description for the node error boundary.
    pub fn context(&self) -> &str {
        &self.context
    }

    /// Named PostgreSQL constraint for typed violation cases.
    pub fn constraint(&self) -> Option<&str> {
        self.constraint.as_deref()
    }

    /// Input field owned by an invalid-input refusal.
    pub const fn field(&self) -> Option<&'static str> {
        self.field
    }

    /// Current revision returned by an optimistic-concurrency refusal.
    pub const fn observed_row_version(&self) -> Option<i32> {
        self.observed_row_version
    }

    pub(crate) fn invalid(context: impl Into<Box<str>>, field: &'static str) -> Self {
        Self {
            type_: AccessErrorType::InvalidInput,
            context: context.into(),
            constraint: None,
            field: Some(field),
            observed_row_version: None,
        }
    }

    pub(crate) fn not_found(context: impl Into<Box<str>>) -> Self {
        Self::new(AccessErrorType::NotFound, context)
    }

    pub(crate) fn concurrency_conflict(
        context: impl Into<Box<str>>,
        observed_row_version: i32,
    ) -> Self {
        Self {
            type_: AccessErrorType::ConcurrencyConflict,
            context: context.into(),
            constraint: None,
            field: None,
            observed_row_version: Some(observed_row_version),
        }
    }

    pub(crate) fn internal(context: impl Into<Box<str>>) -> Self {
        Self::new(AccessErrorType::InternalError, context)
    }

    pub(crate) fn from_statement(context: impl Into<Box<str>>, source: &StatementError) -> Self {
        Self::from_statement_with_constraints(context, source, AllowedConstraints::NONE)
    }

    pub(crate) fn from_statement_with_constraints(
        context: impl Into<Box<str>>,
        source: &StatementError,
        allowed_constraints: AllowedConstraints,
    ) -> Self {
        Self::from_statement_parts(
            context,
            source.kind(),
            source.constraint(),
            allowed_constraints,
        )
    }

    pub(crate) fn from_statement_parts(
        context: impl Into<Box<str>>,
        type_: StatementErrorType,
        constraint: Option<&str>,
        allowed_constraints: AllowedConstraints,
    ) -> Self {
        let (kind, constraint) = classify(type_, constraint, allowed_constraints);
        let mut error = Self::new(kind, context);
        error.constraint = constraint;
        error
    }

    fn new(kind: AccessErrorType, context: impl Into<Box<str>>) -> Self {
        Self {
            type_: kind,
            context: context.into(),
            constraint: None,
            field: None,
            observed_row_version: None,
        }
    }
}

impl fmt::Display for AccessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.type_.literal(), self.context)
    }
}

impl Error for AccessError {}

fn classify(
    type_: StatementErrorType,
    constraint: Option<&str>,
    allowed_constraints: AllowedConstraints,
) -> (AccessErrorType, Option<Box<str>>) {
    match type_ {
        StatementErrorType::SerializationFailure | StatementErrorType::ConnectionUnavailable => {
            (AccessErrorType::Retry, None)
        }
        StatementErrorType::StatementTimeout => (AccessErrorType::Timeout, None),
        StatementErrorType::PermissionDenied => (AccessErrorType::PermissionDenied, None),
        StatementErrorType::ExclusionViolation
            if constraint.is_some_and(|name| allowed_constraints.permits_exclusion(name)) =>
        {
            (
                AccessErrorType::ExclusionViolation,
                Some(constraint.expect("guarded").into()),
            )
        }
        StatementErrorType::UnknownStatement
        | StatementErrorType::StatementContractMismatch
        | StatementErrorType::RowLimitExceeded
        | StatementErrorType::UniqueViolation
        | StatementErrorType::ForeignKeyViolation
        | StatementErrorType::CheckViolation
        | StatementErrorType::ExclusionViolation
        | StatementErrorType::QueryError
        | StatementErrorType::InvalidResult => (AccessErrorType::InternalError, None),
    }
}

#[cfg(test)]
mod tests {
    use super::{AccessErrorType, AllowedConstraints, classify};
    use wamn_postgres_statements::StatementErrorType;

    #[test]
    fn only_contractual_database_classes_cross_the_boundary() {
        for (source, expected) in [
            (
                StatementErrorType::SerializationFailure,
                AccessErrorType::Retry,
            ),
            (
                StatementErrorType::ConnectionUnavailable,
                AccessErrorType::Retry,
            ),
            (
                StatementErrorType::StatementTimeout,
                AccessErrorType::Timeout,
            ),
            (
                StatementErrorType::PermissionDenied,
                AccessErrorType::PermissionDenied,
            ),
            (
                StatementErrorType::CheckViolation,
                AccessErrorType::InternalError,
            ),
        ] {
            assert_eq!(
                classify(source, None, AllowedConstraints::NONE),
                (expected, None)
            );
        }
    }

    #[test]
    fn only_exact_allowed_exclusions_cross_the_boundary() {
        let allowed = AllowedConstraints {
            exclusion: &["allowed_exclusion"],
        };
        assert_eq!(
            classify(
                StatementErrorType::ExclusionViolation,
                Some("allowed_exclusion"),
                allowed
            ),
            (
                AccessErrorType::ExclusionViolation,
                Some("allowed_exclusion".into())
            )
        );
        for constraint in [None, Some("hidden_exclusion"), Some("ALLOWED_EXCLUSION")] {
            assert_eq!(
                classify(StatementErrorType::ExclusionViolation, constraint, allowed),
                (AccessErrorType::InternalError, None)
            );
        }
        for kind in [
            StatementErrorType::UniqueViolation,
            StatementErrorType::ForeignKeyViolation,
            StatementErrorType::CheckViolation,
            StatementErrorType::ExclusionViolation,
        ] {
            assert_eq!(
                classify(kind, Some("allowed_exclusion"), AllowedConstraints::NONE),
                (AccessErrorType::InternalError, None)
            );
            if kind != StatementErrorType::ExclusionViolation {
                assert_eq!(
                    classify(kind, Some("allowed_exclusion"), allowed),
                    (AccessErrorType::InternalError, None)
                );
            }
        }
    }
}
