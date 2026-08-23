//! Reusable exact-credential probe for host-owned PostgreSQL connections.
//!
//! The `WorkloadRoleFamily` lifecycle (`wamn-0h0g.13.59`) is the intended
//! credential minter and the trusted runtime selector (`wamn-0h0g.22.8`) is
//! the intended production caller. This module deliberately does not select a
//! project or authority class, create credentials, grant authority, or install
//! a pool/listener consumer.

use std::fmt::{Debug, Display, Formatter};

use tokio_postgres::{Config, GenericClient};

/// Whether a caller also observed an ambient database credential source.
///
/// Any present ambient source is a conflict. Its value never crosses this
/// boundary, so it cannot override the explicit source or appear in an error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AmbientCredentialState {
    Absent,
    Present,
}

/// The connection whose physical identity is being checked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialConnectionKind {
    Pooled,
    Listener,
}

/// Stable failure category for the exact-credential boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialProbeErrorKind {
    SourceConflict,
    InvalidSource,
    ProbeUnavailable,
    PredicateMismatch,
}

/// Stable predicate that refused the connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialProbePredicate {
    CredentialSource,
    SessionUser,
    CurrentUser,
    Database,
    TenantBinding,
    Membership,
    Acl,
}

/// Contextual refusal that never retains credential material or database
/// error detail.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CredentialProbeError {
    kind: CredentialProbeErrorKind,
    predicate: CredentialProbePredicate,
    connection_kind: Option<CredentialConnectionKind>,
}

impl CredentialProbeError {
    fn source(kind: CredentialProbeErrorKind, predicate: CredentialProbePredicate) -> Self {
        Self {
            kind,
            predicate,
            connection_kind: None,
        }
    }

    fn connection(
        kind: CredentialProbeErrorKind,
        predicate: CredentialProbePredicate,
        connection_kind: CredentialConnectionKind,
    ) -> Self {
        Self {
            kind,
            predicate,
            connection_kind: Some(connection_kind),
        }
    }

    /// Return the stable refusal category.
    pub fn kind(&self) -> CredentialProbeErrorKind {
        self.kind
    }

    /// Return the exact predicate that refused.
    pub fn predicate(&self) -> CredentialProbePredicate {
        self.predicate
    }

    /// Return the checked connection class, if a connection was reached.
    pub fn connection_kind(&self) -> Option<CredentialConnectionKind> {
        self.connection_kind
    }
}

impl Display for CredentialProbeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "database credential exactness refused: {:?}/{:?}",
            self.kind, self.predicate
        )
    }
}

impl std::error::Error for CredentialProbeError {}

/// One explicit parsed database credential plus its trusted tenant binding.
///
/// The binding is lifecycle metadata minted beside the credential, never a
/// guest echo or a caller-settable session GUC. Its provenance belongs to
/// `wamn-0h0g.13.59`; this seam only prevents a selector from pairing that
/// credential with a different expected tenant.
///
/// Debug output is deliberately opaque because `tokio_postgres::Config`
/// contains userinfo and password material.
#[derive(Clone)]
pub struct ExplicitCredentialSource {
    config: Config,
    tenant_binding: Box<str>,
}

impl Debug for ExplicitCredentialSource {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExplicitCredentialSource")
            .field("credential", &"[REDACTED]")
            .field("tenant_binding", &"[REDACTED]")
            .finish()
    }
}

/// Parse the sole admitted credential source.
///
/// The caller reports only whether an ambient source was present; ambient
/// credential bytes are neither accepted nor compared. A present source
/// always refuses, including when it happens to name the same database.
pub fn explicit_credential_source(
    database_url: &str,
    tenant_binding: impl Into<Box<str>>,
    ambient: AmbientCredentialState,
) -> Result<ExplicitCredentialSource, CredentialProbeError> {
    if ambient == AmbientCredentialState::Present {
        return Err(CredentialProbeError::source(
            CredentialProbeErrorKind::SourceConflict,
            CredentialProbePredicate::CredentialSource,
        ));
    }
    let tenant_binding = tenant_binding.into();
    if tenant_binding.is_empty() {
        return Err(CredentialProbeError::source(
            CredentialProbeErrorKind::InvalidSource,
            CredentialProbePredicate::TenantBinding,
        ));
    }
    let config = database_url.parse::<Config>().map_err(|_| {
        CredentialProbeError::source(
            CredentialProbeErrorKind::InvalidSource,
            CredentialProbePredicate::CredentialSource,
        )
    })?;
    if config.get_user().is_none() || config.get_dbname().is_none() {
        return Err(CredentialProbeError::source(
            CredentialProbeErrorKind::InvalidSource,
            CredentialProbePredicate::CredentialSource,
        ));
    }
    Ok(ExplicitCredentialSource {
        config,
        tenant_binding,
    })
}

/// PostgreSQL role relationship checked for the authenticated principal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MembershipMode {
    Member,
    Usage,
    Set,
}

impl MembershipMode {
    const fn as_sql(self) -> &'static str {
        match self {
            Self::Member => "MEMBER",
            Self::Usage => "USAGE",
            Self::Set => "SET",
        }
    }
}

/// One required or forbidden effective role relationship.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MembershipExpectation {
    role: Box<str>,
    mode: MembershipMode,
    granted: bool,
}

impl MembershipExpectation {
    pub fn new(role: impl Into<Box<str>>, mode: MembershipMode, granted: bool) -> Self {
        Self {
            role: role.into(),
            mode,
            granted,
        }
    }
}

/// PostgreSQL object whose effective ACL is checked.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AclTarget {
    Database(Box<str>),
    Schema(Box<str>),
    Table(Box<str>),
    Column {
        relation: Box<str>,
        column: Box<str>,
    },
    Function(Box<str>),
}

/// One required or forbidden effective ACL fact.
///
/// PostgreSQL validates the bound privilege literal for the typed target; an
/// invalid target/privilege pair is a fail-closed unavailable probe.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AclExpectation {
    target: AclTarget,
    privilege: Box<str>,
    granted: bool,
}

impl AclExpectation {
    pub fn new(target: AclTarget, privilege: impl Into<Box<str>>, granted: bool) -> Self {
        Self {
            target,
            privilege: privilege.into(),
            granted,
        }
    }
}

/// Exact facts required of one physical database connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpectedCredentialIdentity {
    session_user: Box<str>,
    current_user: Box<str>,
    database: Box<str>,
    tenant_binding: Box<str>,
    memberships: Vec<MembershipExpectation>,
    acl: Vec<AclExpectation>,
}

impl ExpectedCredentialIdentity {
    pub fn new(
        session_user: impl Into<Box<str>>,
        current_user: impl Into<Box<str>>,
        database: impl Into<Box<str>>,
        tenant_binding: impl Into<Box<str>>,
        memberships: Vec<MembershipExpectation>,
        acl: Vec<AclExpectation>,
    ) -> Self {
        Self {
            session_user: session_user.into(),
            current_user: current_user.into(),
            database: database.into(),
            tenant_binding: tenant_binding.into(),
            memberships,
            acl,
        }
    }
}

/// One explicit credential and the exact connection facts it must produce.
///
/// Both pooled and independently opened listener connections are checked with
/// the same immutable expectation. The caller obtains the parsed config from
/// [`Self::connection_config`] and must probe each new physical connection
/// before use; enforcing that construction sequence belongs to the trusted
/// selector in `wamn-0h0g.22.8`.
///
/// Exactness is source user/database/tenant metadata -> expected identity ->
/// observed session/current user, database, memberships, and ACL. Host/port may
/// legitimately name a proxy or replica, and PostgreSQL cannot report which
/// password authenticated a session, so endpoint and password bytes are
/// intentionally outside the equality proof and never retained here.
pub struct CredentialExactnessProbe {
    source: ExplicitCredentialSource,
    expected: ExpectedCredentialIdentity,
}

impl Debug for CredentialExactnessProbe {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CredentialExactnessProbe")
            .field("credential", &"[REDACTED]")
            .field("expected_identity", &"[REDACTED]")
            .finish()
    }
}

/// Bind one explicit source to the exact identity expected by its trusted
/// selector. User, database, and tenant swaps refuse before a socket is used.
pub fn credential_exactness_probe(
    source: ExplicitCredentialSource,
    expected: ExpectedCredentialIdentity,
) -> Result<CredentialExactnessProbe, CredentialProbeError> {
    if source.config.get_user() != Some(expected.session_user.as_ref()) {
        return Err(CredentialProbeError::source(
            CredentialProbeErrorKind::PredicateMismatch,
            CredentialProbePredicate::SessionUser,
        ));
    }
    if source.config.get_dbname() != Some(expected.database.as_ref()) {
        return Err(CredentialProbeError::source(
            CredentialProbeErrorKind::PredicateMismatch,
            CredentialProbePredicate::Database,
        ));
    }
    if source.tenant_binding != expected.tenant_binding {
        return Err(CredentialProbeError::source(
            CredentialProbeErrorKind::PredicateMismatch,
            CredentialProbePredicate::TenantBinding,
        ));
    }
    Ok(CredentialExactnessProbe { source, expected })
}

impl CredentialExactnessProbe {
    /// Clone the parsed explicit source for one pool or independent listener.
    ///
    /// `Config` contains credentials and must never be logged.
    pub fn connection_config(&self) -> Config {
        self.source.config.clone()
    }

    /// Verify one newly-created physical connection before pooling it.
    pub async fn probe_pooled<C>(&self, client: &C) -> Result<(), CredentialProbeError>
    where
        C: GenericClient + Sync,
    {
        self.probe(client, CredentialConnectionKind::Pooled).await
    }

    /// Verify one independently-opened listener connection before `LISTEN`.
    pub async fn probe_listener<C>(&self, client: &C) -> Result<(), CredentialProbeError>
    where
        C: GenericClient + Sync,
    {
        self.probe(client, CredentialConnectionKind::Listener).await
    }

    async fn probe<C>(
        &self,
        client: &C,
        connection_kind: CredentialConnectionKind,
    ) -> Result<(), CredentialProbeError>
    where
        C: GenericClient + Sync,
    {
        self.probe_queries(&PostgresQueryExecutor { client }, connection_kind)
            .await
    }

    async fn probe_queries(
        &self,
        queries: &impl CredentialProbeQueries,
        connection_kind: CredentialConnectionKind,
    ) -> Result<(), CredentialProbeError> {
        let identity = queries
            .identity()
            .await
            .map_err(|predicate| unavailable(connection_kind, predicate))?;
        verify_identity(&self.expected, &identity, connection_kind)?;

        for membership in &self.expected.memberships {
            let actual = queries
                .membership(membership)
                .await
                .map_err(|predicate| unavailable(connection_kind, predicate))?;
            verify_boolean(
                membership.granted,
                actual,
                CredentialProbePredicate::Membership,
                connection_kind,
            )?;
        }

        for acl in &self.expected.acl {
            let actual = queries
                .acl(acl)
                .await
                .map_err(|predicate| unavailable(connection_kind, predicate))?;
            verify_boolean(
                acl.granted,
                actual,
                CredentialProbePredicate::Acl,
                connection_kind,
            )?;
        }
        Ok(())
    }
}

#[derive(Clone)]
struct ObservedIdentity {
    session_user: String,
    current_user: String,
    database: String,
}

fn verify_identity(
    expected: &ExpectedCredentialIdentity,
    actual: &ObservedIdentity,
    connection_kind: CredentialConnectionKind,
) -> Result<(), CredentialProbeError> {
    for (matches, predicate) in [
        (
            actual.session_user == expected.session_user.as_ref(),
            CredentialProbePredicate::SessionUser,
        ),
        (
            actual.current_user == expected.current_user.as_ref(),
            CredentialProbePredicate::CurrentUser,
        ),
        (
            actual.database == expected.database.as_ref(),
            CredentialProbePredicate::Database,
        ),
    ] {
        if !matches {
            return Err(CredentialProbeError::connection(
                CredentialProbeErrorKind::PredicateMismatch,
                predicate,
                connection_kind,
            ));
        }
    }
    Ok(())
}

fn verify_boolean(
    expected: bool,
    actual: bool,
    predicate: CredentialProbePredicate,
    connection_kind: CredentialConnectionKind,
) -> Result<(), CredentialProbeError> {
    if expected == actual {
        Ok(())
    } else {
        Err(CredentialProbeError::connection(
            CredentialProbeErrorKind::PredicateMismatch,
            predicate,
            connection_kind,
        ))
    }
}

fn unavailable(
    connection_kind: CredentialConnectionKind,
    predicate: CredentialProbePredicate,
) -> CredentialProbeError {
    CredentialProbeError::connection(
        CredentialProbeErrorKind::ProbeUnavailable,
        predicate,
        connection_kind,
    )
}

trait CredentialProbeQueries {
    async fn identity(&self) -> Result<ObservedIdentity, CredentialProbePredicate>;
    async fn membership(
        &self,
        expectation: &MembershipExpectation,
    ) -> Result<bool, CredentialProbePredicate>;
    async fn acl(&self, expectation: &AclExpectation) -> Result<bool, CredentialProbePredicate>;
}

struct PostgresQueryExecutor<'a, C> {
    client: &'a C,
}

impl<C> CredentialProbeQueries for PostgresQueryExecutor<'_, C>
where
    C: GenericClient + Sync,
{
    async fn identity(&self) -> Result<ObservedIdentity, CredentialProbePredicate> {
        let row = self
            .client
            .query_one(
                "SELECT session_user::text, current_user::text, current_database()::text",
                &[],
            )
            .await
            .map_err(|_| CredentialProbePredicate::CredentialSource)?;
        Ok(ObservedIdentity {
            session_user: row
                .try_get(0)
                .map_err(|_| CredentialProbePredicate::SessionUser)?,
            current_user: row
                .try_get(1)
                .map_err(|_| CredentialProbePredicate::CurrentUser)?,
            database: row
                .try_get(2)
                .map_err(|_| CredentialProbePredicate::Database)?,
        })
    }

    async fn membership(
        &self,
        expectation: &MembershipExpectation,
    ) -> Result<bool, CredentialProbePredicate> {
        self.client
            .query_one(
                "SELECT pg_catalog.pg_has_role(current_user, $1::text, $2::text)",
                &[&expectation.role.as_ref(), &expectation.mode.as_sql()],
            )
            .await
            .and_then(|row| row.try_get(0))
            .map_err(|_| CredentialProbePredicate::Membership)
    }

    async fn acl(&self, expectation: &AclExpectation) -> Result<bool, CredentialProbePredicate> {
        let row = match &expectation.target {
            AclTarget::Database(database) => self
                .client
                .query_one(
                    "SELECT pg_catalog.has_database_privilege(current_user, $1::text, $2::text)",
                    &[&database.as_ref(), &expectation.privilege.as_ref()],
                )
                .await,
            AclTarget::Schema(schema) => {
                self.client
                    .query_one(
                        "SELECT pg_catalog.has_schema_privilege(current_user, $1::text, $2::text)",
                        &[&schema.as_ref(), &expectation.privilege.as_ref()],
                    )
                    .await
            }
            AclTarget::Table(relation) => {
                self.client
                    .query_one(
                        "SELECT pg_catalog.has_table_privilege(current_user, $1::text, $2::text)",
                        &[&relation.as_ref(), &expectation.privilege.as_ref()],
                    )
                    .await
            }
            AclTarget::Column { relation, column } => {
                self.client
                    .query_one(
                        "SELECT pg_catalog.has_column_privilege(\
                         current_user, $1::text, $2::text, $3::text)",
                        &[
                            &relation.as_ref(),
                            &column.as_ref(),
                            &expectation.privilege.as_ref(),
                        ],
                    )
                    .await
            }
            AclTarget::Function(function) => self
                .client
                .query_one(
                    "SELECT pg_catalog.has_function_privilege(current_user, $1::text, $2::text)",
                    &[&function.as_ref(), &expectation.privilege.as_ref()],
                )
                .await,
        }
        .map_err(|_| CredentialProbePredicate::Acl)?;
        row.try_get(0).map_err(|_| CredentialProbePredicate::Acl)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    const URL: &str = "postgresql://exact-user:do-not-print@localhost/exact_db";

    fn source(tenant: &str) -> ExplicitCredentialSource {
        explicit_credential_source(URL, tenant, AmbientCredentialState::Absent).unwrap()
    }

    fn expected(tenant: &str) -> ExpectedCredentialIdentity {
        ExpectedCredentialIdentity::new(
            "exact-user",
            "exact-user",
            "exact_db",
            tenant,
            vec![MembershipExpectation::new(
                "wamn_app",
                MembershipMode::Member,
                true,
            )],
            vec![AclExpectation::new(
                AclTarget::Table("app.entity".into()),
                "SELECT",
                true,
            )],
        )
    }

    fn observation() -> ObservedIdentity {
        ObservedIdentity {
            session_user: "exact-user".to_owned(),
            current_user: "exact-user".to_owned(),
            database: "exact_db".to_owned(),
        }
    }

    struct FakeQueries {
        identity: ObservedIdentity,
        membership: bool,
        acl: bool,
        identity_calls: AtomicUsize,
        membership_calls: AtomicUsize,
        acl_calls: AtomicUsize,
    }

    impl FakeQueries {
        fn new(identity: ObservedIdentity, membership: bool, acl: bool) -> Self {
            Self {
                identity,
                membership,
                acl,
                identity_calls: AtomicUsize::new(0),
                membership_calls: AtomicUsize::new(0),
                acl_calls: AtomicUsize::new(0),
            }
        }
    }

    impl CredentialProbeQueries for FakeQueries {
        async fn identity(&self) -> Result<ObservedIdentity, CredentialProbePredicate> {
            self.identity_calls.fetch_add(1, Ordering::Relaxed);
            Ok(self.identity.clone())
        }

        async fn membership(
            &self,
            _expectation: &MembershipExpectation,
        ) -> Result<bool, CredentialProbePredicate> {
            self.membership_calls.fetch_add(1, Ordering::Relaxed);
            Ok(self.membership)
        }

        async fn acl(
            &self,
            _expectation: &AclExpectation,
        ) -> Result<bool, CredentialProbePredicate> {
            self.acl_calls.fetch_add(1, Ordering::Relaxed);
            Ok(self.acl)
        }
    }

    fn assert_mismatch(
        error: CredentialProbeError,
        predicate: CredentialProbePredicate,
        connection_kind: Option<CredentialConnectionKind>,
    ) {
        assert_eq!(error.kind(), CredentialProbeErrorKind::PredicateMismatch);
        assert_eq!(error.predicate(), predicate);
        assert_eq!(error.connection_kind(), connection_kind);
    }

    #[test]
    fn exact_source_rejects_an_ambient_conflict_without_disclosure() {
        let error = explicit_credential_source(URL, "tenant-a", AmbientCredentialState::Present)
            .unwrap_err();
        assert_eq!(error.kind(), CredentialProbeErrorKind::SourceConflict);
        assert_eq!(
            error.predicate(),
            CredentialProbePredicate::CredentialSource
        );
        let rendered = format!("{error:?} {error}");
        for secret in [URL, "exact-user", "do-not-print", "localhost", "exact_db"] {
            assert!(!rendered.contains(secret), "leaked {secret:?}: {rendered}");
        }
    }

    #[test]
    fn source_and_probe_debug_are_redacted() {
        let source = source("tenant-a");
        assert_eq!(
            format!("{source:?}"),
            "ExplicitCredentialSource { credential: \"[REDACTED]\", tenant_binding: \"[REDACTED]\" }"
        );
        let probe = credential_exactness_probe(source, expected("tenant-a")).unwrap();
        let rendered = format!("{probe:?}");
        for secret in ["exact-user", "do-not-print", "tenant-a", "exact_db"] {
            assert!(!rendered.contains(secret), "leaked {secret:?}: {rendered}");
        }
    }

    #[test]
    fn source_user_database_and_tenant_swaps_refuse() {
        let wrong_user = ExpectedCredentialIdentity::new(
            "other-user",
            "other-user",
            "exact_db",
            "tenant-a",
            vec![],
            vec![],
        );
        assert_mismatch(
            credential_exactness_probe(source("tenant-a"), wrong_user).unwrap_err(),
            CredentialProbePredicate::SessionUser,
            None,
        );

        let wrong_database = ExpectedCredentialIdentity::new(
            "exact-user",
            "exact-user",
            "other_db",
            "tenant-a",
            vec![],
            vec![],
        );
        assert_mismatch(
            credential_exactness_probe(source("tenant-a"), wrong_database).unwrap_err(),
            CredentialProbePredicate::Database,
            None,
        );

        assert_mismatch(
            credential_exactness_probe(source("tenant-a"), expected("tenant-b")).unwrap_err(),
            CredentialProbePredicate::TenantBinding,
            None,
        );
    }

    #[tokio::test]
    async fn pooled_identity_refuses_each_wrong_database_fact() {
        for (predicate, mutate) in [
            (
                CredentialProbePredicate::SessionUser,
                (|actual: &mut ObservedIdentity| actual.session_user = "other-user".to_owned())
                    as fn(&mut ObservedIdentity),
            ),
            (
                CredentialProbePredicate::CurrentUser,
                |actual: &mut ObservedIdentity| actual.current_user = "other-user".to_owned(),
            ),
            (
                CredentialProbePredicate::Database,
                |actual: &mut ObservedIdentity| actual.database = "other_db".to_owned(),
            ),
        ] {
            let probe =
                credential_exactness_probe(source("tenant-a"), expected("tenant-a")).unwrap();
            let mut actual = observation();
            mutate(&mut actual);
            let queries = FakeQueries::new(actual, true, true);
            assert_mismatch(
                probe
                    .probe_queries(&queries, CredentialConnectionKind::Pooled)
                    .await
                    .unwrap_err(),
                predicate,
                Some(CredentialConnectionKind::Pooled),
            );
        }
    }

    #[tokio::test]
    async fn membership_and_acl_mismatches_are_distinct_predicates() {
        let probe = credential_exactness_probe(source("tenant-a"), expected("tenant-a")).unwrap();
        assert_mismatch(
            probe
                .probe_queries(
                    &FakeQueries::new(observation(), false, true),
                    CredentialConnectionKind::Pooled,
                )
                .await
                .unwrap_err(),
            CredentialProbePredicate::Membership,
            Some(CredentialConnectionKind::Pooled),
        );
        assert_mismatch(
            probe
                .probe_queries(
                    &FakeQueries::new(observation(), true, false),
                    CredentialConnectionKind::Pooled,
                )
                .await
                .unwrap_err(),
            CredentialProbePredicate::Acl,
            Some(CredentialConnectionKind::Pooled),
        );
    }

    #[tokio::test]
    async fn listener_uses_the_same_expected_identity_contract() {
        let probe = credential_exactness_probe(source("tenant-a"), expected("tenant-a")).unwrap();
        let mut actual = observation();
        actual.session_user = "pooled-user".to_owned();
        assert_mismatch(
            probe
                .probe_queries(
                    &FakeQueries::new(actual, true, true),
                    CredentialConnectionKind::Listener,
                )
                .await
                .unwrap_err(),
            CredentialProbePredicate::SessionUser,
            Some(CredentialConnectionKind::Listener),
        );
    }

    #[tokio::test]
    async fn pooled_and_listener_paths_run_the_same_complete_query_contract() {
        let probe = credential_exactness_probe(source("tenant-a"), expected("tenant-a")).unwrap();
        for connection_kind in [
            CredentialConnectionKind::Pooled,
            CredentialConnectionKind::Listener,
        ] {
            let queries = FakeQueries::new(observation(), true, true);
            probe
                .probe_queries(&queries, connection_kind)
                .await
                .unwrap();
            assert_eq!(queries.identity_calls.load(Ordering::Relaxed), 1);
            assert_eq!(queries.membership_calls.load(Ordering::Relaxed), 1);
            assert_eq!(queries.acl_calls.load(Ordering::Relaxed), 1);
        }
    }
}
