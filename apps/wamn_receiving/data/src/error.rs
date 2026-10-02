use std::error::Error;
use std::fmt;

use wamn_postgres_statements::{StatementError, StatementErrorType};

/// Exact named constraints an operation contract permits callers to observe.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AllowedConstraints {
    unique: &'static [&'static str],
    foreign_key: &'static [&'static str],
    check: &'static [&'static str],
    exclusion: &'static [&'static str],
}

impl AllowedConstraints {
    /// Read operations expose no named constraint violations.
    pub(crate) const NONE: Self = Self {
        unique: &[],
        foreign_key: &[],
        check: &[],
        exclusion: &[],
    };

    /// Build one operation policy from generator-owned constraint slices.
    pub(crate) const fn new(
        unique: &'static [&'static str],
        foreign_key: &'static [&'static str],
        check: &'static [&'static str],
        exclusion: &'static [&'static str],
    ) -> Self {
        Self {
            unique,
            foreign_key,
            check,
            exclusion,
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

    fn permits_exclusion(self, name: &str) -> bool {
        self.exclusion.contains(&name)
    }
}

/// Stable operation-level error class returned by Receiving accessors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessErrorType {
    InvalidInput,
    NotFound,
    ConcurrencyConflict,
    IdempotencyConflict,
    UniqueViolation,
    ForeignKeyViolation,
    CheckViolation,
    ExclusionViolation,
    Retry,
    Timeout,
    PermissionDenied,
    InternalError,
}

impl AccessErrorType {
    /// Frozen operation-contract literal for this error class.
    pub const fn literal(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::NotFound => "not_found",
            Self::ConcurrencyConflict => "concurrency_conflict",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::UniqueViolation => "unique_violation",
            Self::ForeignKeyViolation => "foreign_key_violation",
            Self::CheckViolation => "check_violation",
            Self::ExclusionViolation => "exclusion_violation",
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
    type_: AccessErrorType,
    context: Box<str>,
    constraint: Option<Box<str>>,
    field: Option<&'static str>,
    minimum: Option<i64>,
    maximum: Option<i64>,
    observed: Option<i64>,
    observed_row_version: Option<i32>,
}

impl AccessError {
    /// Stable class; callers must not match display text.
    pub const fn error_type(&self) -> AccessErrorType {
        self.type_
    }

    /// Named PostgreSQL constraint for typed violation cases.
    pub fn constraint(&self) -> Option<&str> {
        self.constraint.as_deref()
    }

    /// Input field owned by an `invalid_input` refusal.
    pub const fn field(&self) -> Option<&'static str> {
        self.field
    }

    /// Optional lower bound owned by an `invalid_input` refusal.
    pub const fn minimum(&self) -> Option<i64> {
        self.minimum
    }

    /// Optional upper bound owned by an `invalid_input` refusal.
    pub const fn maximum(&self) -> Option<i64> {
        self.maximum
    }

    /// Optional observed bound value owned by an `invalid_input` refusal.
    pub const fn observed(&self) -> Option<i64> {
        self.observed
    }

    /// Current row version returned by an optimistic-concurrency refusal.
    pub const fn observed_row_version(&self) -> Option<i32> {
        self.observed_row_version
    }

    pub(crate) fn invalid(context: impl Into<Box<str>>, field: &'static str) -> Self {
        Self::new(AccessErrorType::InvalidInput, context).with_field(field)
    }

    pub(crate) fn not_found(context: impl Into<Box<str>>) -> Self {
        Self::new(AccessErrorType::NotFound, context)
    }

    pub(crate) fn internal(context: impl Into<Box<str>>) -> Self {
        Self::new(AccessErrorType::InternalError, context)
    }

    pub(crate) fn from_statement(
        context: impl Into<Box<str>>,
        source: &StatementError,
        allowed_constraints: AllowedConstraints,
    ) -> Self {
        Self::from_statement_parts(
            context,
            source.type_(),
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
        Self {
            type_: kind,
            context: context.into(),
            constraint,
            field: None,
            minimum: None,
            maximum: None,
            observed: None,
            observed_row_version: None,
        }
    }

    fn new(kind: AccessErrorType, context: impl Into<Box<str>>) -> Self {
        Self {
            type_: kind,
            context: context.into(),
            constraint: None,
            field: None,
            minimum: None,
            maximum: None,
            observed: None,
            observed_row_version: None,
        }
    }

    fn with_field(mut self, field: &'static str) -> Self {
        self.field = Some(field);
        self
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
        StatementErrorType::UniqueViolation
            if constraint.is_some_and(|name| allowed_constraints.permits_unique(name)) =>
        {
            named_violation(
                AccessErrorType::UniqueViolation,
                constraint.expect("guarded"),
            )
        }
        StatementErrorType::ForeignKeyViolation
            if constraint.is_some_and(|name| allowed_constraints.permits_foreign_key(name)) =>
        {
            named_violation(
                AccessErrorType::ForeignKeyViolation,
                constraint.expect("guarded"),
            )
        }
        StatementErrorType::CheckViolation
            if constraint.is_some_and(|name| allowed_constraints.permits_check(name)) =>
        {
            named_violation(
                AccessErrorType::CheckViolation,
                constraint.expect("guarded"),
            )
        }
        StatementErrorType::ExclusionViolation
            if constraint.is_some_and(|name| allowed_constraints.permits_exclusion(name)) =>
        {
            named_violation(
                AccessErrorType::ExclusionViolation,
                constraint.expect("guarded"),
            )
        }
        StatementErrorType::PermissionDenied => (AccessErrorType::PermissionDenied, None),
        StatementErrorType::UniqueViolation
        | StatementErrorType::ForeignKeyViolation
        | StatementErrorType::CheckViolation
        | StatementErrorType::ExclusionViolation
        | StatementErrorType::UnknownStatement
        | StatementErrorType::StatementContractMismatch
        | StatementErrorType::RowLimitExceeded
        | StatementErrorType::QueryError
        | StatementErrorType::InvalidResult => (AccessErrorType::InternalError, None),
    }
}

fn named_violation(kind: AccessErrorType, constraint: &str) -> (AccessErrorType, Option<Box<str>>) {
    (kind, Some(constraint.into()))
}

#[cfg(test)]
mod tests {
    use super::{AccessErrorType, AllowedConstraints, classify};
    use wamn_postgres_statements::StatementErrorType;

    const UPDATE_CONSTRAINTS: AllowedConstraints = AllowedConstraints {
        unique: &["allowed_unique"],
        foreign_key: &["allowed_foreign_key"],
        check: &["allowed_check"],
        exclusion: &["allowed_exclusion"],
    };

    #[test]
    fn reads_hide_all_named_constraint_violations() {
        let errors = [
            (StatementErrorType::UniqueViolation, "hidden_unique"),
            (
                StatementErrorType::ForeignKeyViolation,
                "hidden_foreign_key",
            ),
            (StatementErrorType::CheckViolation, "hidden_check"),
            (StatementErrorType::ExclusionViolation, "hidden_exclusion"),
        ];

        for (kind, constraint) in errors {
            assert_eq!(
                classify(kind, Some(constraint), AllowedConstraints::NONE),
                (AccessErrorType::InternalError, None)
            );
        }
    }

    #[test]
    fn operation_contract_exposes_only_exact_names_of_the_expected_kind() {
        let accepted = [
            (
                (StatementErrorType::UniqueViolation, "allowed_unique"),
                AccessErrorType::UniqueViolation,
            ),
            (
                (
                    StatementErrorType::ForeignKeyViolation,
                    "allowed_foreign_key",
                ),
                AccessErrorType::ForeignKeyViolation,
            ),
            (
                (StatementErrorType::CheckViolation, "allowed_check"),
                AccessErrorType::CheckViolation,
            ),
            (
                (StatementErrorType::ExclusionViolation, "allowed_exclusion"),
                AccessErrorType::ExclusionViolation,
            ),
        ];

        for ((error, expected_constraint), expected_kind) in accepted {
            let (kind, constraint) = classify(error, Some(expected_constraint), UPDATE_CONSTRAINTS);
            assert_eq!(kind, expected_kind);
            assert_eq!(constraint.as_deref(), Some(expected_constraint));
        }

        let rejected = [
            (StatementErrorType::UniqueViolation, "unknown_unique"),
            (
                StatementErrorType::ForeignKeyViolation,
                "unknown_foreign_key",
            ),
            (StatementErrorType::CheckViolation, "unknown_check"),
            (StatementErrorType::CheckViolation, "allowed_unique"),
            (StatementErrorType::ExclusionViolation, "unknown_exclusion"),
            (StatementErrorType::ExclusionViolation, "allowed_unique"),
            (StatementErrorType::UniqueViolation, "allowed_exclusion"),
        ];

        for (kind, constraint) in rejected {
            assert_eq!(
                classify(kind, Some(constraint), UPDATE_CONSTRAINTS),
                (AccessErrorType::InternalError, None)
            );
        }
    }

    #[test]
    fn non_constraint_transport_meanings_are_preserved() {
        let cases = [
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
                StatementErrorType::RowLimitExceeded,
                AccessErrorType::InternalError,
            ),
            (
                StatementErrorType::QueryError,
                AccessErrorType::InternalError,
            ),
        ];

        for constraint in [None, Some("ALLOWED_EXCLUSION")] {
            assert_eq!(
                classify(
                    StatementErrorType::ExclusionViolation,
                    constraint,
                    UPDATE_CONSTRAINTS
                ),
                (AccessErrorType::InternalError, None)
            );
        }

        for (kind, expected_kind) in cases {
            assert_eq!(
                classify(kind, None, AllowedConstraints::NONE),
                (expected_kind, None)
            );
        }
    }

    // The existing exclusion fixture supplies actual PostgreSQL diagnostics.
    #[test]
    #[ignore = "requires: WAMN_EXCLUSION_DIAGNOSTICS"]
    fn generated_update_exclusion_from_postgres() {
        let path = std::env::var_os("WAMN_EXCLUSION_DIAGNOSTICS")
            .expect("WAMN_EXCLUSION_DIAGNOSTICS must name the disposable PostgreSQL diagnostics");
        let diagnostics: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let diagnostic = &diagnostics["receiving"];
        assert_eq!(diagnostic["sqlstate"], "23P01");
        let constraint = diagnostic["constraint"]
            .as_str()
            .expect("server constraint name");
        let update = crate::generated::error::Constraints {
            unique: crate::generated::purchase_order::sql::UPDATE_UNIQUE_CONSTRAINTS,
            foreign_key: crate::generated::purchase_order::sql::UPDATE_FOREIGN_KEY_CONSTRAINTS,
            check: crate::generated::purchase_order::sql::UPDATE_CHECK_CONSTRAINTS,
            exclusion: crate::generated::purchase_order::sql::UPDATE_EXCLUSION_CONSTRAINTS,
        };
        let error = crate::generated::error::Error::from_parts(
            StatementErrorType::ExclusionViolation,
            Some(constraint),
            &update,
            "purchase_order.update",
        );
        assert_eq!(
            error,
            crate::generated::error::Error::ExclusionViolation {
                constraint: "purchase_order_supplier_id_excl".to_owned()
            }
        );
    }
}
