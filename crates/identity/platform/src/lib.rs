//! First-party platform identities live in the T1 system database.
//!
//! This crate owns human and service principals, local human credentials,
//! project-role assignments, and opaque personal access tokens. It deliberately
//! contains no HTTP, cookie, OIDC, JWT, or per-project `app_system` authority:
//! every function here is transport-neutral and takes an already-open client.

use std::fmt;
use std::time::Duration;

use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
};
use password_hash::{
    Error as PasswordHashError,
    rand_core::{OsRng, RngCore as _},
};
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;
use tokio_postgres::{GenericClient, Row, error::SqlState};

#[cfg(test)]
const PRINCIPAL_COLUMNS: &str = "id::text, kind, subject, display_name, status";
#[cfg(test)]
const PAT_COLUMNS: [&str; 6] = [
    "id::text",
    "token_prefix",
    "label",
    "to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')",
    "to_char(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')",
    "to_char(revoked_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')",
];
const INSERT_HUMAN_SQL: &str = "WITH principal AS ( \
    INSERT INTO identity.principals (kind, subject, display_name) \
    VALUES ('human', $1, $2) \
    RETURNING id, kind, subject, display_name, status \
), credential AS ( \
    INSERT INTO identity.local_credentials (principal_id, password_hash) \
    SELECT id, $3 FROM principal RETURNING principal_id \
) SELECT principal.id::text, principal.kind, principal.subject, \
         principal.display_name, principal.status \
    FROM principal JOIN credential ON credential.principal_id = principal.id";
const INSERT_SERVICE_SQL: &str = "INSERT INTO identity.principals \
    (kind, subject, display_name) VALUES ('service', $1, $2) \
    RETURNING id::text, kind, subject, display_name, status";
const SELECT_PRINCIPAL_BY_ID_SQL: &str = "SELECT id::text, kind, subject, \
    display_name, status FROM identity.principals WHERE id = $1::text::uuid";
const SELECT_PRINCIPAL_BY_SUBJECT_SQL: &str = "SELECT id::text, kind, subject, \
    display_name, status FROM identity.principals \
    WHERE kind = $1 AND subject = $2";
const SELECT_HUMAN_CREDENTIAL_SQL: &str = "SELECT p.id::text, p.kind, p.subject, \
    p.display_name, p.status, c.password_hash \
    FROM identity.principals p \
    JOIN identity.local_credentials c ON c.principal_id = p.id \
    WHERE p.kind = 'human' AND p.subject = $1";
const DISABLE_PRINCIPAL_SQL: &str = "UPDATE identity.principals \
    SET status = 'disabled', disabled_at = COALESCE(disabled_at, now()), updated_at = now() \
    WHERE id = $1::text::uuid \
    RETURNING id::text, kind, subject, display_name, status";
const ASSIGN_PROJECT_ROLE_SQL: &str = "INSERT INTO identity.project_roles \
    (principal_id, org, project, role) VALUES ($1::text::uuid, $2, $3, $4) \
    ON CONFLICT DO NOTHING";
const SELECT_PROJECT_ROLES_SQL: &str = "SELECT role FROM identity.project_roles \
    WHERE principal_id = $1::text::uuid AND org = $2 AND project = $3 ORDER BY role";
// Every stored token instant crosses this API as second-resolution RFC 3339 UTC
// text rendered by PostgreSQL, so the crate needs no calendar dependency and a
// later transport can put the value straight on the wire.
const INSERT_PAT_SQL: &str = "INSERT INTO identity.pats \
    (principal_id, token_prefix, token_hash, label, expires_at) \
    SELECT p.id, $2, $3, $4, now() + ($5::bigint * interval '1 second') \
    FROM identity.principals p \
    WHERE p.id = $1::text::uuid AND p.status = 'active' \
    RETURNING id::text, token_prefix, label, \
        to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), \
        to_char(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), \
        to_char(revoked_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')";
const SELECT_PAT_BY_PREFIX_SQL: &str = "SELECT p.id::text, p.kind, p.subject, \
    p.display_name, p.status, identity.pats.token_hash, \
    (identity.pats.revoked_at IS NULL AND identity.pats.expires_at > now()) AS usable \
    FROM identity.pats JOIN identity.principals p \
        ON p.id = identity.pats.principal_id \
    WHERE identity.pats.token_prefix = $1";
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
        to_char(revoked_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')";

/// Maximum accepted principal-subject length.
pub const MAX_SUBJECT_LEN: usize = 254;

/// Maximum accepted display-name length.
pub const MAX_DISPLAY_NAME_LEN: usize = 200;

/// Maximum accepted local-secret length.
pub const MAX_LOCAL_SECRET_LEN: usize = 1024;

/// Maximum accepted role-slug length.
pub const MAX_ROLE_LEN: usize = 64;

/// Maximum accepted personal-access-token label length.
pub const MAX_PAT_LABEL_LEN: usize = 200;

/// Longest accepted personal-access-token lifetime. Expiry is mandatory, so
/// every issued token dies within a year even if nobody revokes it.
pub const MAX_PAT_TTL: Duration = Duration::from_secs(365 * 24 * 60 * 60);

/// Marker every first-party personal access token starts with. A token reads
/// `wamn_pat_<16 hex lookup digits>_<64 hex secret digits>`; the lookup half is
/// stored in the clear as the index key, the whole string only as a digest.
pub const PAT_TOKEN_PREFIX: &str = "wamn_pat_";

/// Random bytes behind the non-secret lookup half of a token.
const PAT_LOOKUP_BYTES: usize = 8;

/// Random bytes behind the secret half of a token.
const PAT_SECRET_BYTES: usize = 32;

/// The kind of first-party platform principal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrincipalKind {
    /// A person with an optional local or federated presenter.
    Human,
    /// A non-human client that authenticates through a machine presenter.
    Service,
}

impl PrincipalKind {
    /// Return the stable database literal.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Service => "service",
        }
    }

    fn parse(value: &str) -> Result<Self, IdentityError> {
        match value {
            "human" => Ok(Self::Human),
            "service" => Ok(Self::Service),
            other => Err(IdentityError::new(
                IdentityErrorKind::CorruptData,
                format!("unknown stored principal kind {other:?}"),
            )),
        }
    }
}

impl fmt::Display for PrincipalKind {
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
                IdentityErrorKind::CorruptData,
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

/// Opaque platform principal identity minted by the system database.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PrincipalId(Box<str>);

impl PrincipalId {
    /// Return the canonical UUID text stored by PostgreSQL.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PrincipalId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A stored first-party principal. This value is identity data, not proof that
/// the current caller authenticated as that principal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    id: PrincipalId,
    kind: PrincipalKind,
    subject: Box<str>,
    display_name: Box<str>,
    status: PrincipalStatus,
}

impl Principal {
    /// Return the opaque principal ID.
    pub fn id(&self) -> &PrincipalId {
        &self.id
    }

    /// Return whether this is a human or service principal.
    pub const fn kind(&self) -> PrincipalKind {
        self.kind
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

/// Proof-bearing principal produced only by an admitted presenter.
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
}

impl AuthenticatedPrincipal {
    /// Return the verified principal record.
    pub fn principal(&self) -> &Principal {
        &self.principal
    }
}

/// One project-scoped role assignment. Role names are opaque canonical slugs;
/// this core does not attach permissions to them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRole {
    role: Box<str>,
}

impl ProjectRole {
    /// Return the stable role slug.
    pub fn as_str(&self) -> &str {
        &self.role
    }
}

impl fmt::Display for ProjectRole {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
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
    token: Box<str>,
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
pub enum IdentityErrorKind {
    /// Caller input violated a local invariant.
    InvalidInput,
    /// A principal with the same kind and subject already exists.
    Conflict,
    /// A referenced principal or project does not exist.
    NotFound,
    /// Stored identity data violated the crate's model.
    CorruptData,
    /// PostgreSQL rejected or failed an operation.
    Database,
    /// Password hashing, parsing, verification, or its blocking task failed.
    PasswordHash,
}

/// Canonical identity-core error with a stable kind and diagnostic message.
#[derive(Debug)]
pub struct IdentityError {
    kind: IdentityErrorKind,
    message: Box<str>,
}

impl IdentityError {
    fn new(kind: IdentityErrorKind, message: impl Into<Box<str>>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Return the stable failure class.
    pub const fn kind(&self) -> IdentityErrorKind {
        self.kind
    }
}

impl fmt::Display for IdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for IdentityError {}

/// Create a human principal and its Argon2id local credential atomically.
pub async fn create_human(
    client: &(impl GenericClient + Sync),
    subject: &str,
    display_name: &str,
    local_secret: &[u8],
) -> Result<Principal, IdentityError> {
    let subject = canonical_subject(subject)?;
    let display_name = checked_display_name(display_name)?;
    validate_local_secret(local_secret)?;
    let owned_secret = local_secret.to_vec();
    let password_hash = tokio::task::spawn_blocking(move || hash_local_secret(&owned_secret))
        .await
        .map_err(|error| {
            IdentityError::new(
                IdentityErrorKind::PasswordHash,
                format!("local credential hashing task failed: {error}"),
            )
        })??;
    let row = client
        .query_one(INSERT_HUMAN_SQL, &[&subject, &display_name, &password_hash])
        .await
        .map_err(database_error)?;
    decode_principal(&row)
}

/// Create a service principal. Machine authentication is supplied by a later
/// presenter and no local password row is created.
pub async fn create_service(
    client: &(impl GenericClient + Sync),
    subject: &str,
    display_name: &str,
) -> Result<Principal, IdentityError> {
    let subject = canonical_subject(subject)?;
    let display_name = checked_display_name(display_name)?;
    let row = client
        .query_one(INSERT_SERVICE_SQL, &[&subject, &display_name])
        .await
        .map_err(database_error)?;
    decode_principal(&row)
}

/// Resolve a principal by its opaque ID without authenticating the caller.
pub async fn resolve_principal(
    client: &(impl GenericClient + Sync),
    id: &PrincipalId,
) -> Result<Option<Principal>, IdentityError> {
    client
        .query_opt(SELECT_PRINCIPAL_BY_ID_SQL, &[&id.as_str()])
        .await
        .map_err(database_error)?
        .as_ref()
        .map(decode_principal)
        .transpose()
}

/// Resolve a principal by kind and normalized subject without authenticating
/// the caller.
pub async fn resolve_subject(
    client: &(impl GenericClient + Sync),
    kind: PrincipalKind,
    subject: &str,
) -> Result<Option<Principal>, IdentityError> {
    let subject = canonical_subject(subject)?;
    client
        .query_opt(SELECT_PRINCIPAL_BY_SUBJECT_SQL, &[&kind.as_str(), &subject])
        .await
        .map_err(database_error)?
        .as_ref()
        .map(decode_principal)
        .transpose()
}

/// Disable a principal. Repeated calls are idempotent and retain identity data.
pub async fn disable_principal(
    client: &(impl GenericClient + Sync),
    id: &PrincipalId,
) -> Result<Principal, IdentityError> {
    let row = client
        .query_opt(DISABLE_PRINCIPAL_SQL, &[&id.as_str()])
        .await
        .map_err(database_error)?
        .ok_or_else(|| {
            IdentityError::new(IdentityErrorKind::NotFound, "principal does not exist")
        })?;
    decode_principal(&row)
}

/// Authenticate an active human with a local secret.
///
/// Unknown subjects, invalid secrets, disabled humans, service principals, and
/// humans without a local presenter all return `Ok(None)`. A transport may map
/// them to one generic refusal without leaking which predicate failed.
pub async fn authenticate_local(
    client: &(impl GenericClient + Sync),
    subject: &str,
    local_secret: &[u8],
) -> Result<Option<AuthenticatedPrincipal>, IdentityError> {
    let subject = canonical_subject(subject)?;
    validate_local_secret(local_secret)?;
    let row = client
        .query_opt(SELECT_HUMAN_CREDENTIAL_SQL, &[&subject])
        .await
        .map_err(database_error)?;

    let Some(row) = row else {
        let owned_secret = local_secret.to_vec();
        tokio::task::spawn_blocking(move || hash_local_secret(&owned_secret))
            .await
            .map_err(|error| {
                IdentityError::new(
                    IdentityErrorKind::PasswordHash,
                    format!("local credential comparison task failed: {error}"),
                )
            })??;
        return Ok(None);
    };

    let principal = decode_principal(&row)?;
    let password_hash: String = row.try_get(5).map_err(database_error)?;
    let owned_secret = local_secret.to_vec();
    let verified =
        tokio::task::spawn_blocking(move || verify_local_secret(&password_hash, &owned_secret))
            .await
            .map_err(|error| {
                IdentityError::new(
                    IdentityErrorKind::PasswordHash,
                    format!("local credential comparison task failed: {error}"),
                )
            })??;

    if !verified || principal.status != PrincipalStatus::Active {
        return Ok(None);
    }
    Ok(Some(AuthenticatedPrincipal { principal }))
}

/// Issue a personal access token for an active principal.
///
/// This is the trusted-context issuance path: the caller must already have
/// authorized the request. It serves humans and services identically and reads
/// no client-supplied identity field — only the principal ID the caller already
/// resolved. A missing or disabled principal is `NotFound`.
pub async fn issue_pat(
    client: &(impl GenericClient + Sync),
    principal_id: &PrincipalId,
    label: &str,
    ttl: Duration,
) -> Result<IssuedPat, IdentityError> {
    let label = checked_pat_label(label)?;
    let ttl_seconds = checked_pat_ttl(ttl)?;
    let (token, prefix) = mint_token();
    let token_hash = digest_token(&token);
    let row = client
        .query_opt(
            INSERT_PAT_SQL,
            &[
                &principal_id.as_str(),
                &prefix,
                &token_hash,
                &label,
                &ttl_seconds,
            ],
        )
        .await
        .map_err(database_error)?
        .ok_or_else(|| {
            IdentityError::new(
                IdentityErrorKind::NotFound,
                "principal does not exist or is disabled",
            )
        })?;
    Ok(IssuedPat {
        token: token.into(),
        record: decode_pat(&row)?,
    })
}

/// Exchange a human's local secret for a personal access token.
///
/// This is the headless `wamn login` flow: [`authenticate_local`] followed by
/// [`issue_pat`] for the principal it proved. Every refusal `authenticate_local`
/// reports stays `Ok(None)` here, so the caller cannot tell which predicate
/// failed.
///
/// # Wire contract
///
/// The reserved management route a later transport mounts carries exactly this
/// shape and nothing more:
///
/// ```text
/// request:  {"subject": "author@example.com", "secret": "…", "label": "laptop"}
/// response: {"token": "wamn_pat_<16 hex>_<64 hex>", "expires_at": "2026-08-07T12:00:00Z"}
/// ```
///
/// `token` appears in that one response and never again, and a refusal carries
/// no field naming the reason. Serialization belongs to the transport, so this
/// crate defines no types for the shape.
pub async fn login_local(
    client: &(impl GenericClient + Sync),
    subject: &str,
    local_secret: &[u8],
    label: &str,
    ttl: Duration,
) -> Result<Option<IssuedPat>, IdentityError> {
    let Some(authenticated) = authenticate_local(client, subject, local_secret).await? else {
        return Ok(None);
    };
    issue_pat(client, authenticated.principal().id(), label, ttl)
        .await
        .map(Some)
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
    let Some(prefix) = lookup_prefix(token) else {
        return Ok(None);
    };
    let row = client
        .query_opt(SELECT_PAT_BY_PREFIX_SQL, &[&prefix])
        .await
        .map_err(database_error)?;
    let Some(row) = row else {
        return Ok(None);
    };

    let principal = decode_principal(&row)?;
    let stored_hash: String = row.try_get(5).map_err(database_error)?;
    let usable: bool = row.try_get(6).map_err(database_error)?;
    if !digest_matches(&stored_hash, &digest_token(token))
        || !usable
        || principal.status != PrincipalStatus::Active
    {
        return Ok(None);
    }
    Ok(Some(AuthenticatedPrincipal { principal }))
}

/// Revoke a personal access token by its non-secret lookup prefix.
///
/// Repeated calls keep the first revocation instant, so a replayed revoke is
/// harmless. An unknown prefix is `NotFound`.
pub async fn revoke_pat(
    client: &(impl GenericClient + Sync),
    prefix: &str,
) -> Result<PatRecord, IdentityError> {
    let prefix = checked_pat_prefix(prefix)?;
    let row = client
        .query_opt(REVOKE_PAT_SQL, &[&prefix])
        .await
        .map_err(database_error)?
        .ok_or_else(|| IdentityError::new(IdentityErrorKind::NotFound, "token does not exist"))?;
    decode_pat(&row)
}

/// Read a principal's tokens, newest first. Only non-secret metadata is stored,
/// so no token material can be returned here.
pub async fn list_pats(
    client: &(impl GenericClient + Sync),
    principal_id: &PrincipalId,
) -> Result<Vec<PatRecord>, IdentityError> {
    let rows = client
        .query(SELECT_PATS_SQL, &[&principal_id.as_str()])
        .await
        .map_err(database_error)?;
    rows.iter().map(decode_pat).collect()
}

/// Assign one opaque role slug to a principal in a registered project.
pub async fn assign_project_role(
    client: &(impl GenericClient + Sync),
    principal_id: &PrincipalId,
    org: &str,
    project: &str,
    role: &str,
) -> Result<(), IdentityError> {
    let org = checked_scope_segment("org", org)?;
    let project = checked_scope_segment("project", project)?;
    let role = canonical_role(role)?;
    client
        .execute(
            ASSIGN_PROJECT_ROLE_SQL,
            &[&principal_id.as_str(), &org, &project, &role],
        )
        .await
        .map_err(database_error)?;
    Ok(())
}

/// Read a principal's role slugs in stable lexical order for one project.
pub async fn project_roles(
    client: &(impl GenericClient + Sync),
    principal_id: &PrincipalId,
    org: &str,
    project: &str,
) -> Result<Vec<ProjectRole>, IdentityError> {
    let org = checked_scope_segment("org", org)?;
    let project = checked_scope_segment("project", project)?;
    let rows = client
        .query(
            SELECT_PROJECT_ROLES_SQL,
            &[&principal_id.as_str(), &org, &project],
        )
        .await
        .map_err(database_error)?;
    rows.into_iter()
        .map(|row| {
            let role: String = row.try_get(0).map_err(database_error)?;
            Ok(ProjectRole { role: role.into() })
        })
        .collect()
}

fn decode_principal(row: &Row) -> Result<Principal, IdentityError> {
    let id: String = row.try_get(0).map_err(database_error)?;
    let kind: String = row.try_get(1).map_err(database_error)?;
    let subject: String = row.try_get(2).map_err(database_error)?;
    let display_name: String = row.try_get(3).map_err(database_error)?;
    let status: String = row.try_get(4).map_err(database_error)?;
    Ok(Principal {
        id: PrincipalId(id.into()),
        kind: PrincipalKind::parse(&kind)?,
        subject: subject.into(),
        display_name: display_name.into(),
        status: PrincipalStatus::parse(&status)?,
    })
}

fn decode_pat(row: &Row) -> Result<PatRecord, IdentityError> {
    let id: String = row.try_get(0).map_err(database_error)?;
    let prefix: String = row.try_get(1).map_err(database_error)?;
    let label: String = row.try_get(2).map_err(database_error)?;
    let created_at: String = row.try_get(3).map_err(database_error)?;
    let expires_at: String = row.try_get(4).map_err(database_error)?;
    let revoked_at: Option<String> = row.try_get(5).map_err(database_error)?;
    Ok(PatRecord {
        id: PatId(id.into()),
        prefix: prefix.into(),
        label: label.into(),
        created_at: created_at.into(),
        expires_at: expires_at.into(),
        revoked_at: revoked_at.map(Into::into),
    })
}

/// Mint a token and return it beside its non-secret lookup prefix.
fn mint_token() -> (String, String) {
    let mut lookup = [0_u8; PAT_LOOKUP_BYTES];
    let mut secret = [0_u8; PAT_SECRET_BYTES];
    OsRng.fill_bytes(&mut lookup);
    OsRng.fill_bytes(&mut secret);
    let prefix = hex::encode(lookup);
    let token = format!("{PAT_TOKEN_PREFIX}{prefix}_{}", hex::encode(secret));
    (token, prefix)
}

/// Digest a whole token. Tokens are uniform 256-bit random material, so a plain
/// SHA-256 is the right rest form — a password KDF would buy nothing against an
/// attacker who cannot enumerate the space anyway.
fn digest_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn digest_matches(stored: &str, presented: &str) -> bool {
    stored.as_bytes().ct_eq(presented.as_bytes()).into()
}

/// Return the lookup half of a well-formed token, or `None` if it is malformed.
fn lookup_prefix(token: &str) -> Option<&str> {
    let (prefix, secret) = token.strip_prefix(PAT_TOKEN_PREFIX)?.split_once('_')?;
    let well_formed = prefix.len() == PAT_LOOKUP_BYTES * 2
        && secret.len() == PAT_SECRET_BYTES * 2
        && is_lower_hex(prefix)
        && is_lower_hex(secret);
    well_formed.then_some(prefix)
}

fn is_lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn checked_pat_label(value: &str) -> Result<String, IdentityError> {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_PAT_LABEL_LEN {
        return Err(IdentityError::new(
            IdentityErrorKind::InvalidInput,
            "token label must contain 1 to 200 bytes",
        ));
    }
    Ok(value.to_owned())
}

fn checked_pat_ttl(ttl: Duration) -> Result<i64, IdentityError> {
    if ttl.as_secs() == 0 || ttl > MAX_PAT_TTL {
        return Err(IdentityError::new(
            IdentityErrorKind::InvalidInput,
            "token lifetime must be 1 second to 365 days",
        ));
    }
    // Bounded by MAX_PAT_TTL above, so the cast cannot truncate.
    Ok(ttl.as_secs() as i64)
}

fn checked_pat_prefix(value: &str) -> Result<&str, IdentityError> {
    if value.len() != PAT_LOOKUP_BYTES * 2 || !is_lower_hex(value) {
        return Err(IdentityError::new(
            IdentityErrorKind::InvalidInput,
            "token prefix must be 16 lowercase hex digits",
        ));
    }
    Ok(value)
}

fn hash_local_secret(secret: &[u8]) -> Result<String, IdentityError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(secret, &salt)
        .map(|hash| hash.to_string())
        .map_err(|error| {
            IdentityError::new(
                IdentityErrorKind::PasswordHash,
                format!("hash local credential with Argon2id: {error}"),
            )
        })
}

fn verify_local_secret(hash: &str, secret: &[u8]) -> Result<bool, IdentityError> {
    let parsed = PasswordHash::new(hash).map_err(|error| {
        IdentityError::new(
            IdentityErrorKind::CorruptData,
            format!("stored local credential is not a PHC string: {error}"),
        )
    })?;
    match Argon2::default().verify_password(secret, &parsed) {
        Ok(()) => Ok(true),
        Err(PasswordHashError::Password) => Ok(false),
        Err(error) => Err(IdentityError::new(
            IdentityErrorKind::CorruptData,
            format!("stored Argon2id credential cannot be verified: {error}"),
        )),
    }
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
            IdentityErrorKind::InvalidInput,
            "principal subject must be a lowercase identity token of at most 254 bytes",
        ));
    }
    Ok(value)
}

fn checked_display_name(value: &str) -> Result<String, IdentityError> {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_DISPLAY_NAME_LEN {
        return Err(IdentityError::new(
            IdentityErrorKind::InvalidInput,
            "display name must contain 1 to 200 bytes",
        ));
    }
    Ok(value.to_owned())
}

fn validate_local_secret(value: &[u8]) -> Result<(), IdentityError> {
    if value.is_empty() || value.len() > MAX_LOCAL_SECRET_LEN {
        return Err(IdentityError::new(
            IdentityErrorKind::InvalidInput,
            "local secret must contain 1 to 1024 bytes",
        ));
    }
    Ok(())
}

fn checked_scope_segment(name: &str, value: &str) -> Result<String, IdentityError> {
    let value = value.trim();
    if value.is_empty() || value.len() > 40 {
        return Err(IdentityError::new(
            IdentityErrorKind::InvalidInput,
            format!("{name} must contain 1 to 40 bytes"),
        ));
    }
    Ok(value.to_owned())
}

fn canonical_role(value: &str) -> Result<String, IdentityError> {
    let value = value.trim().to_ascii_lowercase();
    if value.is_empty()
        || value.len() > MAX_ROLE_LEN
        || !value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || (index > 0 && byte == b'-')
        })
    {
        return Err(IdentityError::new(
            IdentityErrorKind::InvalidInput,
            "role must be a lowercase slug of at most 64 bytes",
        ));
    }
    Ok(value)
}

fn database_error(error: tokio_postgres::Error) -> IdentityError {
    let kind = match error.code() {
        Some(code) if code == &SqlState::UNIQUE_VIOLATION => IdentityErrorKind::Conflict,
        Some(code) if code == &SqlState::FOREIGN_KEY_VIOLATION => IdentityErrorKind::NotFound,
        _ => IdentityErrorKind::Database,
    };
    IdentityError::new(
        kind,
        format!("platform identity database operation failed: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_hash_is_argon2id_and_verifies_only_the_original_secret() {
        let hash = hash_local_secret(b"correct horse battery staple").unwrap();
        assert!(hash.starts_with("$argon2id$v=19$"));
        assert!(verify_local_secret(&hash, b"correct horse battery staple").unwrap());
        assert!(!verify_local_secret(&hash, b"wrong secret").unwrap());
        assert!(!hash.contains("correct horse battery staple"));

        let corrupt_version = hash.replacen("v=19", "v=42", 1);
        let error = verify_local_secret(&corrupt_version, b"correct horse battery staple")
            .expect_err("unsupported stored Argon2 version must be corrupt data");
        assert_eq!(error.kind(), IdentityErrorKind::CorruptData);
    }

    #[test]
    fn subjects_and_roles_are_canonical_fail_closed_tokens() {
        assert_eq!(
            canonical_subject("  USER@example.com ").unwrap(),
            "user@example.com"
        );
        assert_eq!(
            canonical_role(" Project-Author ").unwrap(),
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
                canonical_role(invalid).is_err(),
                "accepted role {invalid:?}"
            );
        }
    }

    #[test]
    fn local_auth_query_is_human_only() {
        assert!(SELECT_HUMAN_CREDENTIAL_SQL.contains("p.kind = 'human'"));
        assert!(SELECT_HUMAN_CREDENTIAL_SQL.contains("identity.local_credentials"));
        assert!(!SELECT_HUMAN_CREDENTIAL_SQL.contains("status = 'active'"));
    }

    fn sample_record() -> PatRecord {
        PatRecord {
            id: PatId("6d3f2d1c-0000-4000-8000-00000000abcd".into()),
            prefix: "0123456789abcdef".into(),
            label: "laptop".into(),
            created_at: "2026-08-07T11:00:00Z".into(),
            expires_at: "2026-08-08T11:00:00Z".into(),
            revoked_at: None,
        }
    }

    #[test]
    fn minted_tokens_are_unique_prefixed_hex_that_parses_back() {
        let (token, prefix) = mint_token();
        let (other, other_prefix) = mint_token();
        assert_ne!(token, other);
        assert_ne!(prefix, other_prefix);
        assert!(token.starts_with(PAT_TOKEN_PREFIX));
        assert_eq!(
            token.len(),
            PAT_TOKEN_PREFIX.len() + PAT_LOOKUP_BYTES * 2 + 1 + PAT_SECRET_BYTES * 2
        );
        assert_eq!(lookup_prefix(&token), Some(prefix.as_str()));
        assert_eq!(checked_pat_prefix(&prefix).unwrap(), prefix);

        let secret = token.rsplit_once('_').unwrap().1;
        for malformed in [
            String::new(),
            "not-a-token".to_owned(),
            token.replacen("wamn_pat_", "wamn_tok_", 1),
            token.to_ascii_uppercase(),
            format!("{PAT_TOKEN_PREFIX}{prefix}_{}", &secret[1..]),
            format!("{PAT_TOKEN_PREFIX}{}_{secret}", &prefix[1..]),
            format!("{PAT_TOKEN_PREFIX}{prefix}{secret}"),
        ] {
            assert!(
                lookup_prefix(&malformed).is_none(),
                "accepted malformed token {malformed:?}"
            );
        }
    }

    #[test]
    fn token_digest_is_deterministic_and_hides_the_token() {
        let (token, _) = mint_token();
        let digest = digest_token(&token);
        assert_eq!(digest, digest_token(&token));
        assert_eq!(digest.len(), 64);
        assert!(is_lower_hex(&digest));
        assert!(!digest.contains(&token));
        assert!(!token.contains(&digest));
        assert!(digest_matches(&digest, &digest_token(&token)));

        let (forged, _) = mint_token();
        assert!(!digest_matches(&digest, &digest_token(&forged)));
        assert!(!digest_matches(&digest, ""));
    }

    #[test]
    fn issued_tokens_never_reach_debug_output() {
        let issued = IssuedPat {
            token: "wamn_pat_0123456789abcdef_secret".into(),
            record: sample_record(),
        };
        let rendered = format!("{issued:?}");
        assert!(!rendered.contains(issued.token()), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(rendered.contains("0123456789abcdef"), "{rendered}");
    }

    #[test]
    fn token_labels_and_lifetimes_fail_closed() {
        assert_eq!(checked_pat_label("  laptop  ").unwrap(), "laptop");
        assert!(checked_pat_label("   ").is_err());
        assert!(checked_pat_label(&"x".repeat(MAX_PAT_LABEL_LEN + 1)).is_err());
        assert_eq!(checked_pat_ttl(Duration::from_secs(3600)).unwrap(), 3600);
        assert!(checked_pat_ttl(Duration::ZERO).is_err());
        assert!(checked_pat_ttl(MAX_PAT_TTL + Duration::from_secs(1)).is_err());
        for invalid in [
            "",
            "0123456789ABCDEF",
            "0123456789abcde",
            "0123456789abcdeg",
        ] {
            assert!(
                checked_pat_prefix(invalid).is_err(),
                "accepted prefix {invalid:?}"
            );
        }
    }

    #[test]
    fn token_storage_keeps_a_digest_and_never_the_token() {
        assert!(INSERT_PAT_SQL.contains("token_hash"));
        for sql in [INSERT_PAT_SQL, SELECT_PAT_BY_PREFIX_SQL, SELECT_PATS_SQL] {
            assert!(!sql.contains("token_secret"));
            assert!(!sql.contains("token_plaintext"));
        }
        assert!(SELECT_PAT_BY_PREFIX_SQL.contains("revoked_at IS NULL"));
        assert!(SELECT_PAT_BY_PREFIX_SQL.contains("expires_at > now()"));
        assert!(REVOKE_PAT_SQL.contains("COALESCE(revoked_at, now())"));
        assert!(INSERT_PAT_SQL.contains("p.status = 'active'"));
        for column in PAT_COLUMNS {
            for sql in [INSERT_PAT_SQL, SELECT_PATS_SQL, REVOKE_PAT_SQL] {
                assert!(sql.contains(column), "missing {column} in {sql}");
            }
        }
    }

    #[test]
    fn principal_columns_are_shared_by_every_record_decoder() {
        for column in PRINCIPAL_COLUMNS.split(", ") {
            let column = column.trim_end_matches("::text");
            assert!(INSERT_SERVICE_SQL.contains(column));
            assert!(SELECT_PRINCIPAL_BY_ID_SQL.contains(column));
            assert!(SELECT_PRINCIPAL_BY_SUBJECT_SQL.contains(column));
            assert!(DISABLE_PRINCIPAL_SQL.contains(column));
        }
    }
}
