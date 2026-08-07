//! First-party platform identities live in the T1 system database.
//!
//! This crate owns human and service principals, local human credentials, and
//! project-role assignments. It deliberately contains no HTTP, cookie, PAT,
//! OIDC, JWT, or per-project `app_system` authority.

use std::fmt;

use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
};
use password_hash::{Error as PasswordHashError, rand_core::OsRng};
use tokio_postgres::{GenericClient, Row, error::SqlState};

#[cfg(test)]
const PRINCIPAL_COLUMNS: &str = "id::text, kind, subject, display_name, status";
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

/// Maximum accepted principal-subject length.
pub const MAX_SUBJECT_LEN: usize = 254;

/// Maximum accepted display-name length.
pub const MAX_DISPLAY_NAME_LEN: usize = 200;

/// Maximum accepted local-secret length.
pub const MAX_LOCAL_SECRET_LEN: usize = 1024;

/// Maximum accepted role-slug length.
pub const MAX_ROLE_LEN: usize = 64;

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
