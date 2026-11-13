//! Reusable exact-credential probe for host-owned PostgreSQL connections.
//!
//! The `WorkloadRoleFamily` lifecycle (`wamn-0h0g.13.59`) is the intended
//! credential minter and the trusted runtime selector (`wamn-0h0g.22.8`) is
//! the intended production caller. This module deliberately does not select a
//! project or authority class, create credentials, grant authority, or install
//! a pool/listener consumer.

use sqlx::Row as _;
use sqlx::postgres::PgConnectOptions as Config;
use std::fmt::{Debug, Display, Formatter};

/// Whether a caller also observed an ambient database credential source.
///
/// Any present ambient source is a conflict. Its value never crosses this
/// boundary, so it cannot override the explicit source or appear in an error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AmbientCredentialState {
    Absent,
    Present,
}

/// The connection whose physical identity is being validated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialConnectionType {
    Pooled,
    Listener,
}

/// Stable failure category for the exact-credential boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialProbeErrorType {
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
    ProjectBinding,
    Membership,
    Acl,
}

/// Contextual refusal that never retains credential material or database
/// error detail.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CredentialProbeError {
    type_: CredentialProbeErrorType,
    predicate: CredentialProbePredicate,
    connection_type: Option<CredentialConnectionType>,
}

impl CredentialProbeError {
    fn source(type_: CredentialProbeErrorType, predicate: CredentialProbePredicate) -> Self {
        Self {
            type_,
            predicate,
            connection_type: None,
        }
    }

    fn connection(
        type_: CredentialProbeErrorType,
        predicate: CredentialProbePredicate,
        connection_type: CredentialConnectionType,
    ) -> Self {
        Self {
            type_,
            predicate,
            connection_type: Some(connection_type),
        }
    }

    /// Return the stable refusal category.
    pub fn error_type(&self) -> CredentialProbeErrorType {
        self.type_
    }

    /// Return the exact predicate that refused.
    pub fn predicate(&self) -> CredentialProbePredicate {
        self.predicate
    }

    /// Return the validated connection class, if a connection was reached.
    pub fn connection_type(&self) -> Option<CredentialConnectionType> {
        self.connection_type
    }
}

impl Display for CredentialProbeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "database credential exactness refused: {:?}/{:?}",
            self.type_, self.predicate
        )
    }
}

impl std::error::Error for CredentialProbeError {}

/// One explicit parsed database credential plus its trusted project binding.
///
/// The binding is lifecycle metadata minted beside the credential, never a
/// guest echo or a caller-settable session GUC. Its provenance belongs to
/// `wamn-0h0g.13.59`; this seam only prevents a selector from pairing that
/// credential with a different expected project.
///
/// Debug output is deliberately opaque because `sqlx::postgres::PgConnectOptions`
/// contains userinfo and password material.
#[derive(Clone)]
pub struct ExplicitCredentialSource {
    config: Config,
    project_env_key_binding: Box<str>,
}

impl Debug for ExplicitCredentialSource {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExplicitCredentialSource")
            .field("credential", &"[REDACTED]")
            .field("project_env_key_binding", &"[REDACTED]")
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
    project_env_key_binding: impl Into<Box<str>>,
    ambient: AmbientCredentialState,
) -> Result<ExplicitCredentialSource, CredentialProbeError> {
    if ambient == AmbientCredentialState::Present {
        return Err(CredentialProbeError::source(
            CredentialProbeErrorType::SourceConflict,
            CredentialProbePredicate::CredentialSource,
        ));
    }
    let project_env_key_binding = project_env_key_binding.into();
    if project_env_key_binding.is_empty() {
        return Err(CredentialProbeError::source(
            CredentialProbeErrorType::InvalidSource,
            CredentialProbePredicate::ProjectBinding,
        ));
    }
    let config = database_url.parse::<Config>().map_err(|_| {
        CredentialProbeError::source(
            CredentialProbeErrorType::InvalidSource,
            CredentialProbePredicate::CredentialSource,
        )
    })?;
    if config.get_username().is_empty() || config.get_database().is_none() {
        return Err(CredentialProbeError::source(
            CredentialProbeErrorType::InvalidSource,
            CredentialProbePredicate::CredentialSource,
        ));
    }
    Ok(ExplicitCredentialSource {
        config,
        project_env_key_binding,
    })
}

/// PostgreSQL role relationship validated for the authenticated principal.
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

/// PostgreSQL object whose effective ACL is validated.
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
    project_env_key_binding: Box<str>,
    memberships: Vec<MembershipExpectation>,
    acl: Vec<AclExpectation>,
}

impl ExpectedCredentialIdentity {
    pub fn new(
        session_user: impl Into<Box<str>>,
        current_user: impl Into<Box<str>>,
        database: impl Into<Box<str>>,
        project_env_key_binding: impl Into<Box<str>>,
        memberships: Vec<MembershipExpectation>,
        acl: Vec<AclExpectation>,
    ) -> Self {
        Self {
            session_user: session_user.into(),
            current_user: current_user.into(),
            database: database.into(),
            project_env_key_binding: project_env_key_binding.into(),
            memberships,
            acl,
        }
    }
}

/// One explicit credential and the exact connection facts it must produce.
///
/// Both pooled and independently opened listener connections are validated with
/// the same immutable expectation. The caller obtains the parsed config from
/// [`Self::connection_config`] and must probe each new physical connection
/// before use; enforcing that construction sequence belongs to the trusted
/// selector in `wamn-0h0g.22.8`.
///
/// Exactness is source user/database/project metadata -> expected identity ->
/// observed session/current user, database, memberships, and ACL. Host/port may
/// legitimately name a proxy or replica, and PostgreSQL cannot report which
/// password authenticated a session, so endpoint and password bytes are
/// intentionally outside the equality comparison and never retained here.
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
/// selector. User, database, and project swaps refuse before a socket is used.
pub fn credential_exactness_probe(
    source: ExplicitCredentialSource,
    expected: ExpectedCredentialIdentity,
) -> Result<CredentialExactnessProbe, CredentialProbeError> {
    if source.config.get_username() != expected.session_user.as_ref() {
        return Err(CredentialProbeError::source(
            CredentialProbeErrorType::PredicateMismatch,
            CredentialProbePredicate::SessionUser,
        ));
    }
    if source.config.get_database() != Some(expected.database.as_ref()) {
        return Err(CredentialProbeError::source(
            CredentialProbeErrorType::PredicateMismatch,
            CredentialProbePredicate::Database,
        ));
    }
    if source.project_env_key_binding != expected.project_env_key_binding {
        return Err(CredentialProbeError::source(
            CredentialProbeErrorType::PredicateMismatch,
            CredentialProbePredicate::ProjectBinding,
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
    pub async fn probe_pooled(
        &self,
        client: &mut sqlx::PgConnection,
    ) -> Result<(), CredentialProbeError> {
        self.probe(client, CredentialConnectionType::Pooled).await
    }

    /// Verify one independently-opened listener connection before `LISTEN`.
    pub async fn probe_listener(
        &self,
        client: &mut sqlx::PgConnection,
    ) -> Result<(), CredentialProbeError> {
        self.probe(client, CredentialConnectionType::Listener).await
    }

    async fn probe(
        &self,
        client: &mut sqlx::PgConnection,
        connection_type: CredentialConnectionType,
    ) -> Result<(), CredentialProbeError> {
        self.probe_queries(&mut PostgresQueryExecutor { client }, connection_type)
            .await
    }

    async fn probe_queries(
        &self,
        queries: &mut impl CredentialProbeQueries,
        connection_type: CredentialConnectionType,
    ) -> Result<(), CredentialProbeError> {
        let identity = queries
            .identity()
            .await
            .map_err(|predicate| unavailable(connection_type, predicate))?;
        verify_identity(&self.expected, &identity, connection_type)?;

        for membership in &self.expected.memberships {
            let actual = queries
                .membership(membership)
                .await
                .map_err(|predicate| unavailable(connection_type, predicate))?;
            verify_boolean(
                membership.granted,
                actual,
                CredentialProbePredicate::Membership,
                connection_type,
            )?;
        }

        for acl in &self.expected.acl {
            let actual = queries
                .acl(acl)
                .await
                .map_err(|predicate| unavailable(connection_type, predicate))?;
            verify_boolean(
                acl.granted,
                actual,
                CredentialProbePredicate::Acl,
                connection_type,
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
    connection_type: CredentialConnectionType,
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
                CredentialProbeErrorType::PredicateMismatch,
                predicate,
                connection_type,
            ));
        }
    }
    Ok(())
}

fn verify_boolean(
    expected: bool,
    actual: bool,
    predicate: CredentialProbePredicate,
    connection_type: CredentialConnectionType,
) -> Result<(), CredentialProbeError> {
    if expected == actual {
        Ok(())
    } else {
        Err(CredentialProbeError::connection(
            CredentialProbeErrorType::PredicateMismatch,
            predicate,
            connection_type,
        ))
    }
}

fn unavailable(
    connection_type: CredentialConnectionType,
    predicate: CredentialProbePredicate,
) -> CredentialProbeError {
    CredentialProbeError::connection(
        CredentialProbeErrorType::ProbeUnavailable,
        predicate,
        connection_type,
    )
}

trait CredentialProbeQueries {
    async fn identity(&mut self) -> Result<ObservedIdentity, CredentialProbePredicate>;
    async fn membership(
        &mut self,
        expectation: &MembershipExpectation,
    ) -> Result<bool, CredentialProbePredicate>;
    async fn acl(&mut self, expectation: &AclExpectation)
    -> Result<bool, CredentialProbePredicate>;
}

struct PostgresQueryExecutor<'a> {
    client: &'a mut sqlx::PgConnection,
}

impl CredentialProbeQueries for PostgresQueryExecutor<'_> {
    async fn identity(&mut self) -> Result<ObservedIdentity, CredentialProbePredicate> {
        let row =
            sqlx::query("SELECT session_user::text, current_user::text, current_database()::text")
                .fetch_one(&mut *self.client)
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
        &mut self,
        expectation: &MembershipExpectation,
    ) -> Result<bool, CredentialProbePredicate> {
        sqlx::query("SELECT pg_catalog.pg_has_role(current_user, $1::text, $2::text)")
            .bind(expectation.role.as_ref())
            .bind(expectation.mode.as_sql())
            .fetch_one(&mut *self.client)
            .await
            .and_then(|row| row.try_get(0))
            .map_err(|_| CredentialProbePredicate::Membership)
    }

    async fn acl(
        &mut self,
        expectation: &AclExpectation,
    ) -> Result<bool, CredentialProbePredicate> {
        let row =
            match &expectation.target {
                AclTarget::Database(database) => sqlx::query(
                    "SELECT pg_catalog.has_database_privilege(current_user, $1::text, $2::text)",
                )
                .bind(database.as_ref())
                .bind(expectation.privilege.as_ref())
                .fetch_one(&mut *self.client)
                .await,
                AclTarget::Schema(schema) => {
                    sqlx::query(
                        "SELECT pg_catalog.has_schema_privilege(current_user, $1::text, $2::text)",
                    )
                    .bind(schema.as_ref())
                    .bind(expectation.privilege.as_ref())
                    .fetch_one(&mut *self.client)
                    .await
                }
                AclTarget::Table(relation) => {
                    sqlx::query(
                        "SELECT pg_catalog.has_table_privilege(current_user, $1::text, $2::text)",
                    )
                    .bind(relation.as_ref())
                    .bind(expectation.privilege.as_ref())
                    .fetch_one(&mut *self.client)
                    .await
                }
                AclTarget::Column { relation, column } => {
                    sqlx::query(
                        "SELECT pg_catalog.has_column_privilege(\
                         current_user, $1::text, $2::text, $3::text)",
                    )
                    .bind(relation.as_ref())
                    .bind(column.as_ref())
                    .bind(expectation.privilege.as_ref())
                    .fetch_one(&mut *self.client)
                    .await
                }
                AclTarget::Function(function) => sqlx::query(
                    "SELECT pg_catalog.has_function_privilege(current_user, $1::text, $2::text)",
                )
                .bind(function.as_ref())
                .bind(expectation.privilege.as_ref())
                .fetch_one(&mut *self.client)
                .await,
            }
            .map_err(|_| CredentialProbePredicate::Acl)?;
        row.try_get(0).map_err(|_| CredentialProbePredicate::Acl)
    }
}
