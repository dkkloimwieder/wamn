use std::error::Error;
use std::fmt;

use wamn_postgres_statements::{StatementError, StatementErrorKind};

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
    /// Every class, so a drift guard can walk the whole vocabulary. A variant
    /// added without being listed here is invisible to that guard.
    #[cfg(test)]
    pub(crate) const ALL: &'static [Self] = &[
        Self::InvalidInput,
        Self::NotFound,
        Self::ConcurrencyConflict,
        Self::UniqueViolation,
        Self::ForeignKeyViolation,
        Self::CheckViolation,
        Self::Retry,
        Self::Timeout,
        Self::PermissionDenied,
        Self::InternalError,
    ];

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
    field: Option<&'static str>,
    minimum: Option<i64>,
    maximum: Option<i64>,
    observed: Option<i64>,
    observed_row_version: Option<i64>,
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
    pub const fn observed_row_version(&self) -> Option<i64> {
        self.observed_row_version
    }

    pub(crate) fn invalid(context: impl Into<Box<str>>, field: &'static str) -> Self {
        Self::new(AccessErrorKind::InvalidInput, context).with_field(field)
    }

    pub(crate) fn invalid_range(
        context: impl Into<Box<str>>,
        field: &'static str,
        minimum: i64,
        maximum: i64,
        observed: i64,
    ) -> Self {
        let mut error = Self::invalid(context, field);
        error.minimum = Some(minimum);
        error.maximum = Some(maximum);
        error.observed = Some(observed);
        error
    }

    pub(crate) fn not_found(context: impl Into<Box<str>>) -> Self {
        Self::new(AccessErrorKind::NotFound, context)
    }

    pub(crate) fn concurrency_conflict(
        context: impl Into<Box<str>>,
        observed_row_version: i64,
    ) -> Self {
        let mut error = Self::new(AccessErrorKind::ConcurrencyConflict, context);
        error.observed_row_version = Some(observed_row_version);
        error
    }

    pub(crate) fn internal(context: impl Into<Box<str>>) -> Self {
        Self::new(AccessErrorKind::InternalError, context)
    }

    pub(crate) fn from_statement(
        context: impl Into<Box<str>>,
        source: &StatementError,
        allowed_constraints: AllowedConstraints,
    ) -> Self {
        let (kind, constraint) = classify(source.kind(), source.constraint(), allowed_constraints);
        Self {
            kind,
            context: context.into(),
            constraint,
            field: None,
            minimum: None,
            maximum: None,
            observed: None,
            observed_row_version: None,
        }
    }

    fn new(kind: AccessErrorKind, context: impl Into<Box<str>>) -> Self {
        Self {
            kind,
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
        StatementErrorKind::UniqueViolation
            if constraint.is_some_and(|name| allowed_constraints.permits_unique(name)) =>
        {
            named_violation(
                AccessErrorKind::UniqueViolation,
                constraint.expect("guarded"),
            )
        }
        StatementErrorKind::ForeignKeyViolation
            if constraint.is_some_and(|name| allowed_constraints.permits_foreign_key(name)) =>
        {
            named_violation(
                AccessErrorKind::ForeignKeyViolation,
                constraint.expect("guarded"),
            )
        }
        StatementErrorKind::CheckViolation
            if constraint.is_some_and(|name| allowed_constraints.permits_check(name)) =>
        {
            named_violation(
                AccessErrorKind::CheckViolation,
                constraint.expect("guarded"),
            )
        }
        StatementErrorKind::UniqueViolation
        | StatementErrorKind::ForeignKeyViolation
        | StatementErrorKind::CheckViolation => (AccessErrorKind::InternalError, None),
        StatementErrorKind::PermissionDenied => (AccessErrorKind::PermissionDenied, None),
        // This package declares no exclusion constraint, and `AccessErrorKind`
        // carries no literal for one. Classifying it here exactly as the host
        // classified it before 23P01 had a variant keeps this guest's behaviour
        // unchanged; giving it a typed refusal is a package-level change.
        StatementErrorKind::UnknownStatement
        | StatementErrorKind::StatementContractMismatch
        | StatementErrorKind::RowLimitExceeded
        | StatementErrorKind::ExclusionViolation
        | StatementErrorKind::QueryError
        | StatementErrorKind::InvalidResult => (AccessErrorKind::InternalError, None),
    }
}

fn named_violation(kind: AccessErrorKind, constraint: &str) -> (AccessErrorKind, Option<Box<str>>) {
    (kind, Some(constraint.into()))
}

#[cfg(test)]
mod tests {
    use super::{AccessErrorKind, AllowedConstraints, classify};
    use wamn_postgres_statements::StatementErrorKind;

    const UPDATE_CONSTRAINTS: AllowedConstraints = AllowedConstraints {
        unique: &["allowed_unique"],
        foreign_key: &["allowed_foreign_key"],
        check: &["allowed_check"],
    };

    #[test]
    fn reads_hide_all_named_constraint_violations() {
        let errors = [
            (StatementErrorKind::UniqueViolation, "hidden_unique"),
            (
                StatementErrorKind::ForeignKeyViolation,
                "hidden_foreign_key",
            ),
            (StatementErrorKind::CheckViolation, "hidden_check"),
        ];

        for (kind, constraint) in errors {
            assert_eq!(
                classify(kind, Some(constraint), AllowedConstraints::NONE),
                (AccessErrorKind::InternalError, None)
            );
        }
    }

    #[test]
    fn operation_contract_exposes_only_exact_names_of_the_expected_kind() {
        let accepted = [
            (
                (StatementErrorKind::UniqueViolation, "allowed_unique"),
                AccessErrorKind::UniqueViolation,
            ),
            (
                (
                    StatementErrorKind::ForeignKeyViolation,
                    "allowed_foreign_key",
                ),
                AccessErrorKind::ForeignKeyViolation,
            ),
            (
                (StatementErrorKind::CheckViolation, "allowed_check"),
                AccessErrorKind::CheckViolation,
            ),
        ];

        for ((error, expected_constraint), expected_kind) in accepted {
            let (kind, constraint) = classify(error, Some(expected_constraint), UPDATE_CONSTRAINTS);
            assert_eq!(kind, expected_kind);
            assert_eq!(constraint.as_deref(), Some(expected_constraint));
        }

        let rejected = [
            (StatementErrorKind::UniqueViolation, "unknown_unique"),
            (
                StatementErrorKind::ForeignKeyViolation,
                "unknown_foreign_key",
            ),
            (StatementErrorKind::CheckViolation, "unknown_check"),
            (StatementErrorKind::CheckViolation, "allowed_unique"),
        ];

        for (kind, constraint) in rejected {
            assert_eq!(
                classify(kind, Some(constraint), UPDATE_CONSTRAINTS),
                (AccessErrorKind::InternalError, None)
            );
        }
    }

    #[test]
    fn non_constraint_transport_meanings_are_preserved() {
        let cases = [
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
                StatementErrorKind::RowLimitExceeded,
                AccessErrorKind::InternalError,
            ),
            (
                StatementErrorKind::QueryError,
                AccessErrorKind::InternalError,
            ),
        ];

        for (kind, expected_kind) in cases {
            assert_eq!(
                classify(kind, None, AllowedConstraints::NONE),
                (expected_kind, None)
            );
        }
    }

    /// Literals this package's operations declare, read from the closed
    /// contracts the generator writes beside the accessors.
    fn declared(operations: &[&str]) -> std::collections::BTreeSet<String> {
        operations
            .iter()
            .flat_map(|operation| {
                let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../../packages/receiving/generated/contracts")
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
            .collect()
    }

    /// The PIN between this crate's hand-copied vocabulary and the generator's.
    ///
    /// The generator owns one platform vocabulary, `AccessOperationErrorLiteral`,
    /// and writes every operation's closed case list from it. Both error enums
    /// here are HAND copies of the part this package uses, and nothing held the
    /// copies to the source: when `exclusion_violation` joined the platform
    /// vocabulary the copies did not grow, and `classify` above had to record
    /// that gap in prose. This test holds them together in both directions.
    #[test]
    fn the_hand_copied_vocabulary_agrees_with_the_generated_contracts() {
        /// Operations this crate implements. `record_receipt` answers with
        /// `RecordReceiptErrorKind`; the rest answer with `AccessErrorKind`.
        const OPERATIONS: &[&str] = &[
            "location/list",
            "purchase_order/get",
            "purchase_order/query",
            "purchase_order/update",
            "receipt/get",
            "receipt/query",
            "receiving/load_receipt_screen",
            "receiving/record_receipt",
        ];
        /// The copy's one live disagreement, held at exactly these three.
        ///
        /// No `packages/receiving` contract declares any of them, yet
        /// `operation::operation_error` puts `AccessErrorKind::literal()`
        /// straight into the wire `code`. They are out of reach today only
        /// because every generated `UPDATE_*_CONSTRAINTS` slice is empty;
        /// naming one constraint in the manifest would put an undeclared code
        /// on the wire of a contract that says `closed: true`. Whether they
        /// should be renamed to a domain literal --- the way the manifest
        /// renames `unique_violation` to `receipt_reference_conflict` --- or
        /// dropped is a package-level decision this test does not take.
        const UNDECLARED: &[&str] = &[
            "unique_violation",
            "foreign_key_violation",
            "check_violation",
        ];

        let declared = declared(OPERATIONS);
        let spelled: std::collections::BTreeSet<&str> = AccessErrorKind::ALL
            .iter()
            .map(|kind| kind.literal())
            .chain(
                crate::record_receipt::RecordReceiptErrorKind::ALL
                    .iter()
                    .map(|kind| kind.literal()),
            )
            .collect();

        for literal in &spelled {
            if UNDECLARED.contains(literal) {
                assert!(
                    !declared.contains(*literal),
                    "{literal} is declared now; drop it from UNDECLARED"
                );
            } else {
                assert!(
                    declared.contains(*literal),
                    "{literal} is spelled here but declared by no contract this crate implements"
                );
            }
        }
        for literal in UNDECLARED {
            assert!(
                spelled.contains(literal),
                "{literal} is no longer spelled here; drop it from UNDECLARED"
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
