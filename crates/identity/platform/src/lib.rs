//! First-party platform identities live in the T1 system database.
//!
//! MVP outcome: management auth.
//!
//! This crate owns user and service principals, org memberships and roles, project-role assignments,
//! project-environment memberships, passwords, invitations, personal access tokens, and session keys.
//! The identity protocol crate owns the session-token profile. This crate contains no HTTP, OIDC,
//! or per-project `platform` authority: every function here is
//! transport-neutral and takes an already-open client. An OIDC adapter may
//! resolve an externally authenticated subject through [`resolve_subject`].
//!
//! Principals, project roles, memberships, and tokens carry record-history
//! stamps. A caller that writes them binds `app.user_id` in the same
//! transaction. Without a bound actor, the write fails with SQLSTATE `55000`.

use std::fmt;
use std::time::Duration;

use tokio_postgres::{GenericClient, Row, Statement, error::SqlState};
use wamn_identity_protocol::secret::PatToken;
use wamn_identity_protocol::{PrincipalId, RoleName};

#[cfg(feature = "test-util")]
pub mod test_database;

pub mod application;
pub mod configuration;
pub mod connection_projection;
pub mod connections;
pub mod control;
pub mod org;
pub mod paging;
pub mod password;
pub mod password_login;
pub mod session_keys;
pub mod session_token;

use wamn_broker::auth_changes::AuthDrops;

const INSERT_USER_SQL: &str = "INSERT INTO identity.principals \
    (type, subject, email, display_name) VALUES ('user', $1, $2, $3) \
    RETURNING id::text, type, subject, display_name, status";
/// The user insert of [`create_or_reuse_user`]. An existing email or subject
/// skips it.
const INSERT_USER_IF_ABSENT_SQL: &str = "INSERT INTO identity.principals \
    (type, subject, email, display_name) VALUES ('user', $1, $2, $3) \
    ON CONFLICT DO NOTHING";
const SELECT_USER_BY_EMAIL_SQL: &str = "SELECT p.id::text, p.status, EXISTS (\
    SELECT 1 FROM identity.password_credentials c WHERE c.principal_id = p.id) \
    FROM identity.principals p WHERE p.type = 'user' AND p.email = $1";
const INSERT_SERVICE_SQL: &str = "INSERT INTO identity.principals \
    (type, subject, display_name) VALUES ('service', $1, $2) \
    RETURNING id::text, type, subject, display_name, status";
const SELECT_PRINCIPAL_BY_ID_SQL: &str = "SELECT id::text, type, subject, \
    display_name, status FROM identity.principals WHERE id = $1::text::uuid";
const SELECT_PRINCIPAL_BY_SUBJECT_SQL: &str = "SELECT id::text, type, subject, \
    display_name, status FROM identity.principals \
    WHERE type = $1 AND subject = $2";
const DISABLE_PRINCIPAL_SQL: &str = "UPDATE identity.principals \
    SET status = 'disabled', disabled_at = COALESCE(disabled_at, now()) \
    WHERE id = $1::text::uuid \
    RETURNING id::text, type, subject, display_name, status";
const ASSIGN_PROJECT_ROLE_SQL: &str = "INSERT INTO identity.project_roles \
    (principal_id, org, project, role) VALUES ($1::text::uuid, $2, $3, $4) \
    ON CONFLICT DO NOTHING";
const SELECT_PROJECT_ROLES_SQL: &str = "SELECT role FROM identity.project_roles \
    WHERE principal_id = $1::text::uuid AND org = $2 AND project = $3 ORDER BY role";
const GRANT_PROJECT_ENV_MEMBERSHIP_SQL: &str = "INSERT INTO identity.project_env_memberships \
    (principal_id, org, project, env) VALUES ($1::text::uuid, $2, $3, $4) \
    ON CONFLICT DO NOTHING";
const SELECT_PROJECT_ENV_MEMBERSHIP_SQL: &str =
    "SELECT identity.has_project_access($1::text::uuid,$2,$4,$3)";
const REVOKE_PROJECT_ENV_MEMBERSHIP_SQL: &str = "DELETE FROM identity.project_env_memberships \
    WHERE principal_id = $1::text::uuid AND org = $2 AND project = $3 AND env = $4";
// Every stored token instant crosses this API as second-resolution RFC 3339 UTC
// text rendered by PostgreSQL, so the crate needs no calendar dependency and a
// later transport can put the value straight on the wire.
const INSERT_PAT_SQL: &str = "INSERT INTO identity.pats \
    (principal_id, principal_type, token_prefix, token_hash, label, expires_at) \
    SELECT p.id, p.type, $2, $3, $4, now() + ($5::bigint * interval '1 second') \
    FROM identity.principals p \
    WHERE p.id = $1::text::uuid AND p.status = 'active' AND p.type = 'service' \
    RETURNING id::text, token_prefix, label, \
        to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), \
        to_char(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), \
        to_char(revoked_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')";
const SELECT_PAT_BY_PREFIX_SQL: &str = "SELECT p.id::text, p.type, p.subject, \
    p.display_name, p.status, identity.pats.token_hash, \
    (identity.pats.revoked_at IS NULL AND identity.pats.expires_at > now()) AS usable, identity.pats.id::text AS pat_id \
    FROM identity.pats JOIN identity.principals p \
        ON p.id = identity.pats.principal_id \
    WHERE identity.pats.token_prefix = $1";
const SELECT_ROUTE_PAT_SQL: &str = "SELECT p.id::text, p.type, p.subject, \
    p.display_name, p.status, identity.pats.token_hash, \
    (identity.pats.revoked_at IS NULL AND identity.pats.expires_at > now()) AS usable, identity.pats.id::text AS pat_id \
    FROM identity.pats JOIN identity.principals p \
        ON p.id = identity.pats.principal_id \
    WHERE identity.pats.token_prefix = $1 AND p.type = 'service' \
        AND EXISTS (SELECT 1 FROM identity.project_roles r \
            WHERE r.principal_id = p.id AND r.org = $2 AND r.project = $3 AND r.role = ANY($4::text[]))";
const SELECT_PATS_SQL: &str = "SELECT id::text, token_prefix, label, \
        to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), \
        to_char(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), \
        to_char(revoked_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') \
    FROM identity.pats WHERE principal_id = $1::text::uuid \
    ORDER BY created_at DESC, id";
const REVOKE_PAT_SQL: &str = "UPDATE identity.pats \
    SET revoked_at = COALESCE(revoked_at, now()) WHERE token_prefix = $1 \
    RETURNING id::text, token_prefix, label, \
        to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), \
        to_char(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), \
        to_char(revoked_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), \
        principal_id::text";

/// Maximum accepted principal-subject length.
pub const MAX_SUBJECT_LEN: usize = 254;

/// Maximum accepted display-name length.
pub const MAX_DISPLAY_NAME_LEN: usize = 200;

/// Maximum accepted personal-access-token label length.
pub const MAX_PAT_LABEL_LEN: usize = 200;

/// Longest accepted personal-access-token lifetime. Expiry is mandatory, so
/// every issued token dies within a year even if nobody revokes it.
pub const MAX_PAT_TTL: Duration = Duration::from_hours(365 * 24);

/// Construct the canonical subject of the operator service principal of one
/// environment.
pub fn operator_subject(
    org: &str,
    project: &str,
    environment: &str,
) -> Result<String, IdentityError> {
    canonical_subject(&wamn_registry::name::operator_subject(
        org,
        project,
        environment,
    ))
}

/// The service subject of one scheduled application in one environment project.
pub fn application_subject(project_env_key: &str, package: &str) -> Result<String, IdentityError> {
    canonical_subject(&wamn_registry::name::application_subject(
        project_env_key,
        package,
    ))
}

/// The type of first-party platform principal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrincipalType {
    /// A user resolved from an external identity or represented by a PAT.
    User,
    /// A non-human client that authenticates through a machine presenter.
    Service,
    /// A platform component that writes in the system database. It cannot
    /// authenticate.
    Platform,
}

impl PrincipalType {
    /// Return the stable database literal.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Service => "service",
            Self::Platform => "platform",
        }
    }

    fn parse(value: &str) -> Result<Self, IdentityError> {
        match value {
            "user" => Ok(Self::User),
            "service" => Ok(Self::Service),
            "platform" => Ok(Self::Platform),
            other => Err(IdentityError::new(
                IdentityErrorType::CorruptData,
                format!("unknown stored principal type {other:?}"),
            )),
        }
    }
}

impl fmt::Display for PrincipalType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Whether a stored principal may authenticate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrincipalStatus {
    /// The principal may authenticate through an admitted presenter.
    Active,
    /// The principal is retained but cannot authenticate.
    Disabled,
}

impl PrincipalStatus {
    /// Return the stable database literal.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Disabled => "disabled",
        }
    }

    fn parse(value: &str) -> Result<Self, IdentityError> {
        match value {
            "active" => Ok(Self::Active),
            "disabled" => Ok(Self::Disabled),
            other => Err(IdentityError::new(
                IdentityErrorType::CorruptData,
                format!("unknown stored principal status {other:?}"),
            )),
        }
    }
}

impl fmt::Display for PrincipalStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A stored first-party principal. This identity record does not authenticate
/// the current caller as that principal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    id: PrincipalId,
    type_: PrincipalType,
    subject: Box<str>,
    display_name: Box<str>,
    status: PrincipalStatus,
}

impl Principal {
    /// Return the opaque principal ID.
    pub fn id(&self) -> &PrincipalId {
        &self.id
    }

    /// Return whether this is a user, service, or platform principal.
    pub const fn principal_type(&self) -> PrincipalType {
        self.type_
    }

    /// Return the normalized first-party subject.
    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// Return the mutable presentation label.
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Return whether this principal is active or disabled.
    pub const fn status(&self) -> PrincipalStatus {
        self.status
    }
}

/// An authenticated principal produced only by an admitted presenter.
///
/// It has no public constructor and deliberately implements no deserialization
/// trait. Transport adapters therefore cannot turn a request field directly
/// into trusted invocation context.
///
/// ```compile_fail
/// use wamn_platform_identity::AuthenticatedPrincipal;
/// let forged = AuthenticatedPrincipal { principal: todo!() };
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedPrincipal {
    principal: Principal,
    pat_id: Option<String>,
}

impl AuthenticatedPrincipal {
    /// The source PAT identity, present only after PAT authentication.
    pub fn pat_id(&self) -> Option<&str> {
        self.pat_id.as_deref()
    }

    /// Return the verified principal record.
    pub fn principal(&self) -> &Principal {
        &self.principal
    }
}

/// Opaque personal-access-token identity minted by the system database.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PatId(Box<str>);

impl PatId {
    /// Return the canonical UUID text stored by PostgreSQL.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PatId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Everything one stored personal access token keeps. No field is secret:
/// storage holds a digest of the token and this non-secret lookup metadata, so
/// a record is safe to list, log, and audit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatRecord {
    id: PatId,
    prefix: Box<str>,
    label: Box<str>,
    created_at: Box<str>,
    expires_at: Box<str>,
    revoked_at: Option<Box<str>>,
}

impl PatRecord {
    /// Return the opaque token ID that audit records reference.
    pub fn id(&self) -> &PatId {
        &self.id
    }

    /// Return the non-secret lookup half, which also addresses revocation.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Return the operator-supplied label.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Return the issuance instant as RFC 3339 UTC text.
    pub fn created_at(&self) -> &str {
        &self.created_at
    }

    /// Return the mandatory expiry instant as RFC 3339 UTC text.
    pub fn expires_at(&self) -> &str {
        &self.expires_at
    }

    /// Return the revocation instant as RFC 3339 UTC text, if revoked.
    pub fn revoked_at(&self) -> Option<&str> {
        self.revoked_at.as_deref()
    }
}

/// A freshly minted token: the bearer string plus the record that was stored.
///
/// The bearer string exists only in this value. Storage keeps a digest, so it
/// cannot be recovered afterwards, and this type deliberately implements no
/// `Display`, no equality (which would compare the secret in variable time),
/// and redacts the token from its `Debug` output.
#[derive(Clone)]
pub struct IssuedPat {
    token: zeroize::Zeroizing<String>,
    record: PatRecord,
}

impl IssuedPat {
    /// Return the bearer token. Hand it to the requester once and drop it;
    /// never log, audit, or persist this string.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Return the non-secret record persisted for this token.
    pub fn record(&self) -> &PatRecord {
        &self.record
    }
}

impl fmt::Debug for IssuedPat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IssuedPat")
            .field("token", &"<redacted>")
            .field("record", &self.record)
            .finish()
    }
}

/// Stable classes of identity-core failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityErrorType {
    /// Caller input violated a local invariant.
    InvalidInput,
    /// A principal with the same type and subject already exists.
    Conflict,
    /// A referenced principal or project does not exist.
    NotFound,
    /// Stored identity data violated the crate's model.
    CorruptData,
    /// PostgreSQL rejected or failed an operation.
    Database,
    /// The operating system could not supply secure random bytes.
    Entropy,
}

/// Why a write refused the caller's input, for a caller that answers each
/// refusal with its own code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityRefusal {
    /// The input member of this name is not valid.
    Invalid(&'static str),
    /// The project is not a project of the org.
    ProjectNotFound,
    /// The project is a project of the org already.
    ProjectExists,
    /// The environment is not an environment of the org.
    EnvironmentNotFound,
    /// The principal is not an active member of the org.
    UserNotActive,
    /// The principal is not a member of the org, or not a user.
    UserNotFound,
    /// `org-admin` or `project-admin` covers the grant, so it stays.
    AdminCovered,
}

/// Canonical identity-core error with a stable type and diagnostic message.
#[derive(Debug)]
pub struct IdentityError {
    type_: IdentityErrorType,
    message: Box<str>,
    refusal: Option<IdentityRefusal>,
}

impl IdentityError {
    fn new(type_: IdentityErrorType, message: impl Into<Box<str>>) -> Self {
        Self {
            type_,
            message: message.into(),
            refusal: None,
        }
    }

    fn refused(
        type_: IdentityErrorType,
        refusal: IdentityRefusal,
        message: impl Into<Box<str>>,
    ) -> Self {
        Self {
            refusal: Some(refusal),
            ..Self::new(type_, message)
        }
    }

    /// Return the stable failure class.
    pub const fn error_type(&self) -> IdentityErrorType {
        self.type_
    }

    /// Why the input was refused, when a write names it.
    pub const fn refusal(&self) -> Option<IdentityRefusal> {
        self.refusal
    }
}

impl fmt::Display for IdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for IdentityError {}

/// Translate protocol input errors at the identity boundary.
impl From<wamn_identity_protocol::ParseIdentityError> for IdentityError {
    fn from(source: wamn_identity_protocol::ParseIdentityError) -> Self {
        if source.field() == "principal_id" {
            Self::refused(
                IdentityErrorType::InvalidInput,
                IdentityRefusal::Invalid("principal_id"),
                source.to_string(),
            )
        } else {
            Self::new(IdentityErrorType::InvalidInput, source.to_string())
        }
    }
}

impl From<wamn_identity_protocol::secret::TokenError> for IdentityError {
    fn from(source: wamn_identity_protocol::secret::TokenError) -> Self {
        Self::new(IdentityErrorType::Entropy, source.to_string())
    }
}

impl From<wamn_identity_protocol::SessionError> for IdentityError {
    fn from(source: wamn_identity_protocol::SessionError) -> Self {
        Self::new(IdentityErrorType::InvalidInput, source.to_string())
    }
}

/// Create a passwordless user principal for an externally authenticated
/// subject.
///
/// `email` is the user's deliverable address. It is separate from `subject`,
/// which authenticates and carries no promise of delivery.
/// `reconcile-run-plane` copies the email into the `platform."user"` user
/// row of every project this user can write in (`wamn-0h0g.9.18`), so a user
/// principal always has one.
pub async fn create_user(
    client: &(impl GenericClient + Sync),
    subject: &str,
    email: &str,
    display_name: &str,
) -> Result<Principal, IdentityError> {
    let subject = canonical_subject(subject)?;
    let email = require_email(email)?;
    let display_name = require_display_name(display_name)?;
    let row = client
        .query_one(INSERT_USER_SQL, &[&subject, &email, &display_name])
        .await
        .map_err(|error| database_error(&error))?;
    decode_principal(&row)
}

/// The user that [`create_or_reuse_user`] names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserAccount {
    /// The user principal.
    pub principal_id: PrincipalId,
    /// Whether the user has a password credential.
    pub enrolled: bool,
}

/// Create the user principal of `email`, or reuse the user that has it.
///
/// The email folds as [`require_email`] folds it, and the folded email is
/// also the subject, admitted by [`canonical_subject`]. A reused user keeps
/// its display name. A disabled user is refused. The caller binds the actor
/// in the same transaction.
pub async fn create_or_reuse_user(
    client: &(impl GenericClient + Sync),
    email: &str,
    display_name: &str,
) -> Result<UserAccount, IdentityError> {
    let email = require_email(email)?;
    let subject = canonical_subject(&email)?;
    let display_name = require_display_name(display_name)?;
    client
        .execute(
            INSERT_USER_IF_ABSENT_SQL,
            &[&subject, &email, &display_name],
        )
        .await
        .map_err(|error| database_error(&error))?;
    let row = client
        .query_opt(SELECT_USER_BY_EMAIL_SQL, &[&email])
        .await
        .map_err(|error| database_error(&error))?
        .ok_or_else(|| {
            IdentityError::new(
                IdentityErrorType::Conflict,
                "the subject of this email belongs to another principal",
            )
        })?;
    let principal_id: PrincipalId = row
        .try_get::<_, String>(0)
        .map_err(|error| database_error(&error))?
        .parse()?;
    let status: String = row.try_get(1).map_err(|error| database_error(&error))?;
    if status != "active" {
        return Err(IdentityError::new(
            IdentityErrorType::Conflict,
            format!("principal {principal_id} is disabled"),
        ));
    }
    Ok(UserAccount {
        principal_id,
        enrolled: row.try_get(2).map_err(|error| database_error(&error))?,
    })
}

/// Create a service principal. Machine authentication is supplied by a later
/// presenter.
pub async fn create_service(
    client: &(impl GenericClient + Sync),
    subject: &str,
    display_name: &str,
    drops: &mut AuthDrops,
) -> Result<Principal, IdentityError> {
    let subject = canonical_subject(subject)?;
    let display_name = require_display_name(display_name)?;
    let row = client
        .query_one(INSERT_SERVICE_SQL, &[&subject, &display_name])
        .await
        .map_err(|error| database_error(&error))?;
    let principal = decode_principal(&row)?;
    drops.identity(principal.id().to_string().as_str());
    Ok(principal)
}

/// Resolve a principal by its opaque ID without authenticating the caller.
pub async fn resolve_principal(
    client: &(impl GenericClient + Sync),
    id: &PrincipalId,
) -> Result<Option<Principal>, IdentityError> {
    client
        .query_opt(SELECT_PRINCIPAL_BY_ID_SQL, &[&id.to_string().as_str()])
        .await
        .map_err(|error| database_error(&error))?
        .as_ref()
        .map(decode_principal)
        .transpose()
}

/// Resolve a principal by type and normalized subject without authenticating
/// the caller.
pub async fn resolve_subject(
    client: &(impl GenericClient + Sync),
    type_: PrincipalType,
    subject: &str,
) -> Result<Option<Principal>, IdentityError> {
    let subject = canonical_subject(subject)?;
    client
        .query_opt(
            SELECT_PRINCIPAL_BY_SUBJECT_SQL,
            &[&type_.as_str(), &subject],
        )
        .await
        .map_err(|error| database_error(&error))?
        .as_ref()
        .map(decode_principal)
        .transpose()
}

/// Disable a principal. Repeated calls are idempotent and retain identity data.
pub async fn disable_principal(
    client: &(impl GenericClient + Sync),
    id: &PrincipalId,
    drops: &mut AuthDrops,
) -> Result<Principal, IdentityError> {
    let row = client
        .query_opt(DISABLE_PRINCIPAL_SQL, &[&id.to_string().as_str()])
        .await
        .map_err(|error| database_error(&error))?
        .ok_or_else(|| {
            IdentityError::new(IdentityErrorType::NotFound, "principal does not exist")
        })?;
    drops.identity(id.to_string().as_str());
    decode_principal(&row)
}

/// Issue a personal access token for an active service principal.
///
/// This is the trusted-context issuance path: the caller must already have
/// authorized the request. It reads only the principal ID the caller resolved.
/// A missing, disabled or non-service principal is `NotFound`.
pub async fn issue_pat(
    client: &(impl GenericClient + Sync),
    principal_id: &PrincipalId,
    label: &str,
    ttl: Duration,
) -> Result<IssuedPat, IdentityError> {
    let label = require_pat_label(label)?;
    let ttl_seconds = require_pat_ttl(ttl)?;
    let token = PatToken::mint()?;
    let prefix = token.lookup();
    let token_hash = token.digest();
    let row = client
        .query_opt(
            INSERT_PAT_SQL,
            &[
                &principal_id.to_string().as_str(),
                &prefix,
                &token_hash,
                &label,
                &ttl_seconds,
            ],
        )
        .await
        .map_err(|error| database_error(&error))?
        .ok_or_else(|| {
            IdentityError::new(
                IdentityErrorType::NotFound,
                "principal does not exist or is disabled",
            )
        })?;
    Ok(IssuedPat {
        token: token.into_secret(),
        record: decode_pat(&row)?,
    })
}

/// Authenticate the bearer of a personal access token.
///
/// Malformed tokens, unknown lookup prefixes, forged secrets, expired tokens,
/// revoked tokens, and disabled principals all return `Ok(None)`. Only
/// infrastructure failure is an `Err`, so a transport may map every refusal to
/// one generic response without leaking which predicate failed.
pub async fn authenticate_pat(
    client: &(impl GenericClient + Sync),
    token: &str,
) -> Result<Option<AuthenticatedPrincipal>, IdentityError> {
    let Ok(parsed) = token.parse::<PatToken>() else {
        return Ok(None);
    };
    let prefix = parsed.lookup();
    let row = client
        .query_opt(SELECT_PAT_BY_PREFIX_SQL, &[&prefix])
        .await
        .map_err(|error| database_error(&error))?;
    decide_pat(row, &parsed)
}

/// Apply the token predicates to the looked-up row.
///
/// Shared by the ad-hoc and the prepared paths so the two can never disagree
/// about which token is acceptable: a predicate added here binds both.
fn decide_pat(
    row: Option<Row>,
    token: &PatToken,
) -> Result<Option<AuthenticatedPrincipal>, IdentityError> {
    let Some(row) = row else {
        return Ok(None);
    };

    let principal = decode_principal(&row)?;
    // The pats foreign key refuses a platform principal, so a stored token
    // for one is corrupt data. It never authenticates.
    if principal.type_ == PrincipalType::Platform {
        return Err(IdentityError::new(
            IdentityErrorType::CorruptData,
            "stored token names a platform principal",
        ));
    }
    let stored_hash: String = row.try_get(5).map_err(|error| database_error(&error))?;
    let usable: bool = row.try_get(6).map_err(|error| database_error(&error))?;
    if !token.matches(&stored_hash) || !usable || principal.status != PrincipalStatus::Active {
        return Ok(None);
    }
    Ok(Some(AuthenticatedPrincipal {
        principal,
        pat_id: Some(
            row.try_get("pat_id")
                .map_err(|error| database_error(&error))?,
        ),
    }))
}

/// Revoke a personal access token by its non-secret lookup prefix.
///
/// Repeated calls keep the first revocation instant, so a replayed revoke is
/// harmless. An unknown prefix is `NotFound`.
pub async fn revoke_pat(
    client: &(impl GenericClient + Sync),
    prefix: &str,
    drops: &mut AuthDrops,
) -> Result<PatRecord, IdentityError> {
    let prefix = require_pat_prefix(prefix)?;
    let row = client
        .query_opt(REVOKE_PAT_SQL, &[&prefix])
        .await
        .map_err(|error| database_error(&error))?
        .ok_or_else(|| IdentityError::new(IdentityErrorType::NotFound, "token does not exist"))?;
    let principal: String = row.try_get(6).map_err(|error| database_error(&error))?;
    drops.identity(&principal);
    decode_pat(&row)
}

/// Read a principal's tokens, newest first. Only non-secret metadata is stored,
/// so no token material can be returned here.
pub async fn list_pats(
    client: &(impl GenericClient + Sync),
    principal_id: &PrincipalId,
) -> Result<Vec<PatRecord>, IdentityError> {
    let rows = client
        .query(SELECT_PATS_SQL, &[&principal_id.to_string().as_str()])
        .await
        .map_err(|error| database_error(&error))?;
    rows.iter().map(decode_pat).collect()
}

/// Assign one opaque role slug to a principal in a registered project.
pub async fn assign_project_role(
    client: &(impl GenericClient + Sync),
    principal_id: &PrincipalId,
    org: &str,
    project: &str,
    role: &str,
    drops: &mut AuthDrops,
) -> Result<(), IdentityError> {
    let org = require_target_segment("org", org)?;
    let service: bool = client.query_one(
        "SELECT EXISTS (SELECT 1 FROM identity.principals WHERE id=$1::text::uuid AND type='service')",
        &[&principal_id.to_string().as_str()],
    ).await.map_err(|error| database_error(&error))?.get(0);
    if !service {
        org::active_member(client, principal_id, &org).await?;
    }

    let project = require_target_segment("project", project)?;
    let role = role.parse::<RoleName>()?.to_string();
    let written = client
        .execute(
            ASSIGN_PROJECT_ROLE_SQL,
            &[&principal_id.to_string().as_str(), &org, &project, &role],
        )
        .await
        .map_err(|error| database_error(&error))?;
    if written > 0 {
        drops.identity(principal_id.to_string().as_str());
    }
    Ok(())
}

/// Read a principal's role slugs in stable lexical order for one project.
pub async fn project_roles(
    client: &(impl GenericClient + Sync),
    principal_id: &PrincipalId,
    org: &str,
    project: &str,
) -> Result<Vec<RoleName>, IdentityError> {
    let org = require_target_segment("org", org)?;
    let project = require_target_segment("project", project)?;
    let rows = client
        .query(
            SELECT_PROJECT_ROLES_SQL,
            &[&principal_id.to_string().as_str(), &org, &project],
        )
        .await
        .map_err(|error| database_error(&error))?;
    decode_project_roles(rows)
}

fn decode_project_roles(rows: Vec<Row>) -> Result<Vec<RoleName>, IdentityError> {
    rows.into_iter()
        .map(|row| {
            let role: String = row.try_get(0).map_err(|error| database_error(&error))?;
            Ok(role.parse()?)
        })
        .collect()
}

/// Grant a user principal membership in one registered project-environment.
///
/// Repeated grants are harmless. The database rejects service principals and
/// missing environments. Membership does not create project users or assign roles.
pub async fn grant_project_env_membership(
    client: &(impl GenericClient + Sync),
    principal_id: &PrincipalId,
    org: &str,
    project: &str,
    env: &str,
) -> Result<(), IdentityError> {
    let org = org::active_member(client, principal_id, org).await?;
    let project = require_target_segment("project", project)?;
    let env = require_target_segment("env", env)?;
    client
        .execute(
            GRANT_PROJECT_ENV_MEMBERSHIP_SQL,
            &[&principal_id.to_string().as_str(), &org, &project, &env],
        )
        .await
        .map_err(|error| database_error(&error))?;
    Ok(())
}

/// Read the explicit membership fact for one principal and project-environment.
///
/// Credential authentication must separately verify that the principal is active.
/// Project management roles and memberships in other environments do not count.
pub async fn has_project_env_membership(
    client: &(impl GenericClient + Sync),
    principal_id: &PrincipalId,
    org: &str,
    project: &str,
    env: &str,
) -> Result<bool, IdentityError> {
    let org = require_target_segment("org", org)?;
    let project = require_target_segment("project", project)?;
    let env = require_target_segment("env", env)?;
    client
        .query_one(
            SELECT_PROJECT_ENV_MEMBERSHIP_SQL,
            &[&principal_id.to_string().as_str(), &org, &project, &env],
        )
        .await
        .map_err(|error| database_error(&error))?
        .try_get(0)
        .map_err(|error| database_error(&error))
}

/// Revoke membership, returning whether an explicit grant was removed.
pub async fn revoke_project_env_membership(
    client: &(impl GenericClient + Sync),
    principal_id: &PrincipalId,
    org: &str,
    project: &str,
    env: &str,
    drops: &mut AuthDrops,
) -> Result<bool, IdentityError> {
    let org = require_target_segment("org", org)?;
    org::lock_membership(client, principal_id, &org).await?;
    let project = require_target_segment("project", project)?;
    let env = require_target_segment("env", env)?;
    let removed = client
        .execute(
            REVOKE_PROJECT_ENV_MEMBERSHIP_SQL,
            &[&principal_id.to_string().as_str(), &org, &project, &env],
        )
        .await
        .map_err(|error| database_error(&error))?;
    drops.identity(principal_id.to_string().as_str());
    Ok(removed != 0)
}

/// The identity query used by route authentication, prepared once.
///
/// The free functions above hand `query`/`query_opt` a `&str`. `tokio-postgres`
/// converts a `&str` through `prepare::prepare` on every call and never caches
/// it (`prepare_cached` belongs to `deadpool_postgres::Object`, and the route's
/// identity reader is a bare `Client`), so each read cost a Parse+Describe round
/// trip before its Bind+Execute. The PAT and project-role reads were measured
/// at 1.184 ms and 0.709 ms per request against a 0.30 ms single round trip: see
/// the measurement record in commit `b40a1714b8a0`.
///
/// Holding the `Statement` handle moves that Parse to process start. The
/// handle is bound to the connection it was prepared on, so this type must
/// be used only with the client it was prepared from; a different connection
/// would refuse the statement name.
///
/// Route authentication uses [`Self::authenticate_route_pat`] to read both
/// the PAT and its project role or environment membership in one statement.
#[derive(Clone, Debug)]
pub struct PreparedIdentityReads {
    route_pat: Statement,
}

impl PreparedIdentityReads {
    /// Parse the identity statement on `client`, once.
    pub async fn prepare(client: &(impl GenericClient + Sync)) -> Result<Self, IdentityError> {
        Ok(Self {
            route_pat: client
                .prepare(SELECT_ROUTE_PAT_SQL)
                .await
                .map_err(|error| database_error(&error))?,
        })
    }

    /// Authenticate a route PAT and its system target in one round trip.
    ///
    /// Services need one of the accepted project roles. The caller must still
    /// enforce the configured service identity and read operation permissions.
    pub async fn authenticate_route_pat(
        &self,
        client: &(impl GenericClient + Sync),
        token: &str,
        org: &str,
        project: &str,
        accepted_roles: &[&str],
    ) -> Result<Option<AuthenticatedPrincipal>, IdentityError> {
        let Ok(parsed) = token.parse::<PatToken>() else {
            return Ok(None);
        };
        let prefix = parsed.lookup();
        let org = require_target_segment("org", org)?;
        let project = require_target_segment("project", project)?;
        let roles = accepted_roles
            .iter()
            .map(|role| role.parse::<RoleName>().map(|value| value.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        let row = client
            .query_opt(&self.route_pat, &[&prefix, &org, &project, &roles])
            .await
            .map_err(|error| database_error(&error))?;
        decide_pat(row, &parsed)
    }
}

fn decode_principal(row: &Row) -> Result<Principal, IdentityError> {
    let id: String = row.try_get(0).map_err(|error| database_error(&error))?;
    let type_: String = row.try_get(1).map_err(|error| database_error(&error))?;
    let subject: String = row.try_get(2).map_err(|error| database_error(&error))?;
    let display_name: String = row.try_get(3).map_err(|error| database_error(&error))?;
    let status: String = row.try_get(4).map_err(|error| database_error(&error))?;
    Ok(Principal {
        id: id.parse()?,
        type_: PrincipalType::parse(&type_)?,
        subject: subject.into(),
        display_name: display_name.into(),
        status: PrincipalStatus::parse(&status)?,
    })
}

fn decode_pat(row: &Row) -> Result<PatRecord, IdentityError> {
    let id: String = row.try_get(0).map_err(|error| database_error(&error))?;
    let prefix: String = row.try_get(1).map_err(|error| database_error(&error))?;
    let label: String = row.try_get(2).map_err(|error| database_error(&error))?;
    let created_at: String = row.try_get(3).map_err(|error| database_error(&error))?;
    let expires_at: String = row.try_get(4).map_err(|error| database_error(&error))?;
    let revoked_at: Option<String> = row.try_get(5).map_err(|error| database_error(&error))?;
    Ok(PatRecord {
        id: PatId(id.into()),
        prefix: prefix.into(),
        label: label.into(),
        created_at: created_at.into(),
        expires_at: expires_at.into(),
        revoked_at: revoked_at.map(Into::into),
    })
}

fn require_pat_label(value: &str) -> Result<String, IdentityError> {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_PAT_LABEL_LEN {
        return Err(IdentityError::new(
            IdentityErrorType::InvalidInput,
            "token label must contain 1 to 200 bytes",
        ));
    }
    Ok(value.to_owned())
}

fn require_pat_ttl(ttl: Duration) -> Result<i64, IdentityError> {
    if ttl.as_secs() == 0 || ttl > MAX_PAT_TTL {
        return Err(IdentityError::new(
            IdentityErrorType::InvalidInput,
            "token lifetime must be 1 second to 365 days",
        ));
    }
    // Bounded by MAX_PAT_TTL above, so the cast cannot wrap.
    Ok(ttl.as_secs().cast_signed())
}

fn require_pat_prefix(value: &str) -> Result<&str, IdentityError> {
    if !PatToken::valid_lookup(value) {
        return Err(IdentityError::new(
            IdentityErrorType::InvalidInput,
            "token prefix must be 16 lowercase hex digits",
        ));
    }
    Ok(value)
}

fn canonical_subject(value: &str) -> Result<String, IdentityError> {
    let value = value.trim().to_ascii_lowercase();
    if value.is_empty()
        || value.len() > MAX_SUBJECT_LEN
        || !value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (index > 0 && matches!(byte, b'.' | b'_' | b'@' | b'+' | b'-'))
        })
    {
        return Err(IdentityError::new(
            IdentityErrorType::InvalidInput,
            "principal subject must be a lowercase identity token of at most 254 bytes",
        ));
    }
    Ok(value)
}

/// Accept the same user email `principals_email_check` accepts: one `@`, no
/// space on either side, a dot in the domain, and at most 254 bytes. The rule
/// is deliberately loose, because the address is delivered to, not parsed.
///
/// The whole address folds to lower case, exactly as [`canonical_subject`]
/// folds a subject. By the mail standard the local part is case-sensitive, so
/// this deployment folds more than the standard allows. That is a decision and
/// not an oversight. One rule beside an identical rule beats two rules that
/// split the field, and `UNIQUE (email)` then catches `A@x.test` and
/// `a@x.test` as the one address a reader already reads them as. The fold is
/// Unicode and not ASCII, because `principals_email_check` admits a non-ASCII
/// address and a narrower fold would split that address in two.
fn require_email(value: &str) -> Result<String, IdentityError> {
    let value = value.trim().to_lowercase();
    let Some((local, domain)) = value.split_once('@') else {
        return Err(invalid_email());
    };
    if value.len() > MAX_SUBJECT_LEN
        || local.is_empty()
        || domain.is_empty()
        || value.contains(char::is_whitespace)
        || domain.contains('@')
        || !domain.contains('.')
        || domain.starts_with('.')
        || domain.ends_with('.')
    {
        return Err(invalid_email());
    }
    Ok(value)
}

fn invalid_email() -> IdentityError {
    IdentityError::refused(
        IdentityErrorType::InvalidInput,
        IdentityRefusal::Invalid("email"),
        "email must be a local part, an @, and a dotted domain, in at most 254 bytes",
    )
}

fn require_display_name(value: &str) -> Result<String, IdentityError> {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_DISPLAY_NAME_LEN {
        return Err(IdentityError::refused(
            IdentityErrorType::InvalidInput,
            IdentityRefusal::Invalid("display_name"),
            "display name must contain 1 to 200 bytes",
        ));
    }
    Ok(value.to_owned())
}

/// Refuse the email and display name of a user that the identity rules
/// refuse, before a caller asks identity to create the user.
pub fn require_user_contact(email: &str, display_name: &str) -> Result<(), IdentityError> {
    require_email(email)?;
    require_display_name(display_name)?;
    Ok(())
}

fn require_target_segment(name: &'static str, value: &str) -> Result<String, IdentityError> {
    let value = value.trim();
    if value.is_empty() || value.len() > 40 {
        return Err(IdentityError::refused(
            IdentityErrorType::InvalidInput,
            IdentityRefusal::Invalid(name),
            format!("{name} must contain 1 to 40 bytes"),
        ));
    }
    Ok(value.to_owned())
}

fn database_error(error: &tokio_postgres::Error) -> IdentityError {
    let type_ = match error.code() {
        Some(code) if code == &SqlState::UNIQUE_VIOLATION => IdentityErrorType::Conflict,
        Some(code) if code == &SqlState::FOREIGN_KEY_VIOLATION => IdentityErrorType::NotFound,
        _ => IdentityErrorType::Database,
    };
    // `tokio_postgres::Error` displays as the bare string "db error", so the
    // readable half is taken from its database error. That value's own Display
    // carries PostgreSQL's DETAIL, which repeats the offending key, so the
    // message and the constraint name are read directly instead. An operator
    // learns which rule refused the write, and the row's data stays out of the
    // log.
    let reason = error.as_db_error().map_or_else(
        || error.to_string(),
        |database| match database.constraint() {
            Some(constraint) => format!("{} ({constraint})", database.message()),
            None => database.message().to_owned(),
        },
    );
    IdentityError::new(
        type_,
        format!("platform identity database operation failed: {reason}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn application_subject_is_targeted_to_project_env_key_and_package() {
        assert_ne!(
            application_subject("acme--prod--erp", "integration").unwrap(),
            application_subject("acme--dev--erp", "integration").unwrap()
        );
        assert_ne!(
            application_subject("acme--prod--erp", "integration").unwrap(),
            application_subject("acme--prod--erp", "consumer").unwrap()
        );
        assert!(application_subject("bad project", "integration").is_err());
    }

    #[test]
    fn principal_type_literals_round_trip() {
        for type_ in [
            PrincipalType::User,
            PrincipalType::Service,
            PrincipalType::Platform,
        ] {
            assert_eq!(PrincipalType::parse(type_.as_str()).unwrap(), type_);
        }
        assert_eq!(PrincipalType::Platform.as_str(), "platform");
        assert_eq!(
            PrincipalType::parse("robot").unwrap_err().error_type(),
            IdentityErrorType::CorruptData
        );
    }

    #[test]
    fn subjects_and_roles_are_canonical_fail_closed_tokens() {
        assert_eq!(
            canonical_subject("  USER@example.com ").unwrap(),
            "user@example.com"
        );
        assert_eq!(
            " Project-Author ".parse::<RoleName>().unwrap().to_string(),
            "project-author"
        );
        for invalid in ["", "two words", "/root", "-leading"] {
            assert!(
                canonical_subject(invalid).is_err(),
                "accepted subject {invalid:?}"
            );
        }
        for invalid in ["", "two words", "_admin", "-admin"] {
            assert!(
                invalid.parse::<RoleName>().is_err(),
                "accepted role {invalid:?}"
            );
        }
    }

    /// A user email folds to lower case in full, like a subject, so two
    /// spellings of one address become one value (`wamn-0h0g.9.18`). The
    /// `UNIQUE (email)` in `deploy/sql/system-schema.sql` then refuses
    /// the second user, and `identity_live` pins that refusal.
    #[test]
    fn a_user_email_folds_to_one_lowercase_address() {
        assert_eq!(
            require_email("  Case.User@Example.Test ").unwrap(),
            "case.user@example.test"
        );
        assert_eq!(
            require_email("CASE.USER@EXAMPLE.TEST").unwrap(),
            require_email("case.user@example.test").unwrap()
        );
        for invalid in [
            "",
            "no-at-sign",
            "no@domaindot",
            "two words@example.test",
            "@example.test",
            "user@",
            "one@two@example.test",
        ] {
            assert!(
                require_email(invalid).is_err(),
                "accepted email {invalid:?}"
            );
        }
    }

    #[test]
    fn operator_subject_is_exactly_environment_targeted_and_validated() {
        assert_ne!(
            operator_subject("demo", "widgets", "prod").unwrap(),
            operator_subject("demo", "widgets", "dev").unwrap()
        );
        assert!(operator_subject("demo", "two words", "prod").is_err());
    }

    #[test]
    fn token_labels_and_lifetimes_fail_closed() {
        assert_eq!(require_pat_label("  laptop  ").unwrap(), "laptop");
        assert!(require_pat_label("   ").is_err());
        assert!(require_pat_label(&"x".repeat(MAX_PAT_LABEL_LEN + 1)).is_err());
        assert_eq!(require_pat_ttl(Duration::from_secs(3600)).unwrap(), 3600);
        assert!(require_pat_ttl(Duration::ZERO).is_err());
        assert!(require_pat_ttl(MAX_PAT_TTL + Duration::from_secs(1)).is_err());
        for invalid in [
            "",
            "0123456789ABCDEF",
            "0123456789abcde",
            "0123456789abcdeg",
        ] {
            assert!(
                require_pat_prefix(invalid).is_err(),
                "accepted prefix {invalid:?}"
            );
        }
    }
}
