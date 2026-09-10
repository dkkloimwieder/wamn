use std::error::Error;
use std::fmt;

use wamn_postgres_statements::{StatementError, StatementErrorKind};

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
pub enum AccessErrorKind {
    InvalidInput,
    NotFound,
    ConcurrencyConflict,
    ExclusionViolation,
    Retry,
    Timeout,
    PermissionDenied,
    InternalError,
}

impl AccessErrorKind {
    /// Every class, so a drift guard can walk the whole vocabulary. A variant
    /// added without being listed here is invisible to that guard.
    #[cfg(test)]
    pub(crate) const ALL: &'static [Self] = &[
        Self::InvalidInput,
        Self::NotFound,
        Self::ConcurrencyConflict,
        Self::ExclusionViolation,
        Self::Retry,
        Self::Timeout,
        Self::PermissionDenied,
        Self::InternalError,
    ];

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
    kind: AccessErrorKind,
    context: Box<str>,
    constraint: Option<Box<str>>,
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

    /// Named PostgreSQL constraint for typed violation cases.
    pub fn constraint(&self) -> Option<&str> {
        self.constraint.as_deref()
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
            constraint: None,
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
            constraint: None,
            field: None,
            observed_row_version: Some(observed_row_version),
        }
    }

    pub(crate) fn internal(context: impl Into<Box<str>>) -> Self {
        Self::new(AccessErrorKind::InternalError, context)
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
        kind: StatementErrorKind,
        constraint: Option<&str>,
        allowed_constraints: AllowedConstraints,
    ) -> Self {
        let (kind, constraint) = classify(kind, constraint, allowed_constraints);
        let mut error = Self::new(kind, context);
        error.constraint = constraint;
        error
    }

    fn new(kind: AccessErrorKind, context: impl Into<Box<str>>) -> Self {
        Self {
            kind,
            context: context.into(),
            constraint: None,
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

fn classify(
    kind: StatementErrorKind,
    constraint: Option<&str>,
    allowed_constraints: AllowedConstraints,
) -> (AccessErrorKind, Option<Box<str>>) {
    match kind {
        StatementErrorKind::SerializationFailure | StatementErrorKind::ConnectionUnavailable => {
            (AccessErrorKind::Retry, None)
        }
        StatementErrorKind::StatementTimeout => (AccessErrorKind::Timeout, None),
        StatementErrorKind::PermissionDenied => (AccessErrorKind::PermissionDenied, None),
        StatementErrorKind::ExclusionViolation
            if constraint.is_some_and(|name| allowed_constraints.permits_exclusion(name)) =>
        {
            (
                AccessErrorKind::ExclusionViolation,
                Some(constraint.expect("guarded").into()),
            )
        }
        StatementErrorKind::UnknownStatement
        | StatementErrorKind::StatementContractMismatch
        | StatementErrorKind::RowLimitExceeded
        | StatementErrorKind::UniqueViolation
        | StatementErrorKind::ForeignKeyViolation
        | StatementErrorKind::CheckViolation
        | StatementErrorKind::ExclusionViolation
        | StatementErrorKind::QueryError
        | StatementErrorKind::InvalidResult => (AccessErrorKind::InternalError, None),
    }
}

#[cfg(test)]
mod tests {
    use super::{AccessErrorKind, AllowedConstraints, classify};
    use wamn_postgres_statements::StatementErrorKind;

    #[test]
    fn only_contractual_database_classes_cross_the_boundary() {
        for (source, expected) in [
            (
                StatementErrorKind::SerializationFailure,
                AccessErrorKind::Retry,
            ),
            (
                StatementErrorKind::ConnectionUnavailable,
                AccessErrorKind::Retry,
            ),
            (
                StatementErrorKind::StatementTimeout,
                AccessErrorKind::Timeout,
            ),
            (
                StatementErrorKind::PermissionDenied,
                AccessErrorKind::PermissionDenied,
            ),
            (
                StatementErrorKind::CheckViolation,
                AccessErrorKind::InternalError,
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
                StatementErrorKind::ExclusionViolation,
                Some("allowed_exclusion"),
                allowed
            ),
            (
                AccessErrorKind::ExclusionViolation,
                Some("allowed_exclusion".into())
            )
        );
        for constraint in [None, Some("hidden_exclusion"), Some("ALLOWED_EXCLUSION")] {
            assert_eq!(
                classify(StatementErrorKind::ExclusionViolation, constraint, allowed),
                (AccessErrorKind::InternalError, None)
            );
        }
        for kind in [
            StatementErrorKind::UniqueViolation,
            StatementErrorKind::ForeignKeyViolation,
            StatementErrorKind::CheckViolation,
            StatementErrorKind::ExclusionViolation,
        ] {
            assert_eq!(
                classify(kind, Some("allowed_exclusion"), AllowedConstraints::NONE),
                (AccessErrorKind::InternalError, None)
            );
            if kind != StatementErrorKind::ExclusionViolation {
                assert_eq!(
                    classify(kind, Some("allowed_exclusion"), allowed),
                    (AccessErrorKind::InternalError, None)
                );
            }
        }
    }

    /// The PIN between this crate's hand-copied vocabulary and the generator's.
    ///
    /// The generator owns one platform vocabulary,
    /// `AccessOperationErrorLiteral`, and writes every operation's closed case
    /// list from it. This enum is a HAND copy of the part the overlay uses, and
    /// nothing held the copy to the source: when `exclusion_violation` joined
    /// the platform vocabulary the copy did not grow. This test holds them
    /// together in both directions.
    #[test]
    fn the_hand_copied_vocabulary_agrees_with_the_generated_contracts() {
        /// Operations this crate implements. The package also re-declares the
        /// inherited `receiving/record_receipt`, which this crate does not
        /// serve, so its contract is not one this vocabulary must cover.
        const OPERATIONS: &[&str] = &[
            "purchase_order/get",
            "purchase_order/update",
            "quality/approve_inspection",
            "quality/create_inspection",
            "quality/load_purchase_order_detail",
        ];

        let declared: std::collections::BTreeSet<String> = OPERATIONS
            .iter()
            .flat_map(|operation| {
                let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../../packages/client_acme_receiving/generated/contracts")
                    .join(format!("{operation}.errors.json"));
                let contract: serde_json::Value = serde_json::from_slice(
                    &std::fs::read(&path)
                        .unwrap_or_else(|error| panic!("{}: {error}", path.display())),
                )
                .expect("parses");
                contract["cases"]
                    .as_array()
                    .expect("cases")
                    .iter()
                    .map(|case| case["literal"].as_str().expect("literal").to_owned())
                    .collect::<Vec<_>>()
            })
            .collect();
        let exclusions = crate::generated::purchase_order::UPDATE_EXCLUSION_CONSTRAINTS;
        let update: serde_json::Value = serde_json::from_slice(
            &std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(
                "../../../packages/client_acme_receiving/generated/contracts/purchase_order/update.errors.json",
            ))
            .unwrap(),
        )
        .unwrap();
        let declared_exclusions: std::collections::BTreeSet<&str> = update["cases"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|case| case["literal"] == "exclusion_violation")
            .map(|case| case["constraint"].as_str().unwrap())
            .collect();
        assert_eq!(
            exclusions
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            declared_exclusions,
            "the generated exclusion slice and contract disagree"
        );
        let spelled: std::collections::BTreeSet<&str> = AccessErrorKind::ALL
            .iter()
            .filter(|kind| **kind != AccessErrorKind::ExclusionViolation || !exclusions.is_empty())
            .map(|kind| kind.literal())
            .collect();

        for literal in &spelled {
            assert!(
                declared.contains(*literal),
                "{literal} is spelled here but declared by no contract this crate implements"
            );
        }
        for literal in &declared {
            assert!(
                spelled.contains(literal.as_str()),
                "{literal} is declared by a contract this crate implements, \
                 but no error kind here spells it"
            );
        }
    }
}
