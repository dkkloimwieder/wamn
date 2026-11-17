//! First-party platform identities live in the T1 system database.
//!
//! MVP outcome: management auth.
//!
//! This crate owns user principals, org memberships and roles, project-role assignments,
//! project-environment memberships, passwords, invitations, and session keys.
//! The identity protocol crate owns the session-token profile. This crate contains no HTTP, OIDC,
//! or per-project `platform` authority: every function here is
//! transport-neutral and takes an already-open client.
//!
//! Principals, project roles, memberships, and tokens carry record-history
//! stamps. A caller that writes them binds its principal in `platform.tx_identity` in the same
//! transaction. Without a bound actor, the write fails with SQLSTATE `55000`.

use sqlx::Row as _;
use sqlx::postgres::PgRow as Row;
use std::fmt;

use wamn_identity_protocol::{PrincipalId, RoleName};

#[cfg(feature = "test-util")]
pub mod test_database;

pub mod control;
pub mod org;
pub mod paging;
pub mod password;
pub mod password_login;
pub mod session_keys;
pub mod session_token;
pub mod transaction;

const INSERT_USER_SQL: &str = "INSERT INTO identity.principals \
    (type, subject, email, display_name) VALUES ('user', $1, $2, $3) \
    RETURNING id, type, subject, display_name, status";
/// The user insert of [`create_or_reuse_user`]. An existing email or subject
/// skips it.
const INSERT_USER_IF_ABSENT_SQL: &str = "INSERT INTO identity.principals \
    (type, subject, email, display_name) VALUES ('user', $1, $2, $3) \
    ON CONFLICT DO NOTHING";
const SELECT_USER_BY_EMAIL_SQL: &str = "SELECT p.id, p.status, EXISTS (\
    SELECT 1 FROM identity.password_credentials c WHERE c.principal_id = p.id) \
    FROM identity.principals p WHERE p.type = 'user' AND p.email = $1";
const SELECT_PRINCIPAL_BY_ID_SQL: &str = "SELECT id, type, subject, \
    display_name, status FROM identity.principals WHERE id = $1";
const DISABLE_PRINCIPAL_SQL: &str = "UPDATE identity.principals \
    SET status = 'disabled', disabled_at = COALESCE(disabled_at, now()) \
    WHERE id = $1 \
    RETURNING id, type, subject, display_name, status";
const ASSIGN_PROJECT_ROLE_SQL: &str = "INSERT INTO identity.project_roles \
    (principal_id, org, project, role) VALUES ($1, $2, $3, $4) \
    ON CONFLICT DO NOTHING";
const SELECT_PROJECT_ROLES_SQL: &str = "SELECT role FROM identity.project_roles \
    WHERE principal_id = $1 AND org = $2 AND project = $3 ORDER BY role";
const GRANT_PROJECT_ENV_MEMBERSHIP_SQL: &str = "INSERT INTO identity.project_env_memberships \
    (principal_id, org, project, env) VALUES ($1, $2, $3, $4) \
    ON CONFLICT DO NOTHING";
const SELECT_PROJECT_ENV_MEMBERSHIP_SQL: &str = "SELECT identity.has_project_access($1,$2,$4,$3)";
const REVOKE_PROJECT_ENV_MEMBERSHIP_SQL: &str = "DELETE FROM identity.project_env_memberships \
    WHERE principal_id = $1 AND org = $2 AND project = $3 AND env = $4";
/// Maximum accepted principal-subject length.
pub const MAX_SUBJECT_LEN: usize = 254;

/// Maximum accepted display-name length.
pub const MAX_DISPLAY_NAME_LEN: usize = 200;

/// The type of first-party platform principal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrincipalType {
    /// A user of the first-party identity service.
    User,
    /// A platform component that writes in the system database. It cannot
    /// authenticate.
    Platform,
}

impl PrincipalType {
    /// Return the stable database literal.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Platform => "platform",
        }
    }

    fn parse(value: &str) -> Result<Self, IdentityError> {
        match value {
            "user" => Ok(Self::User),
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

    /// Return whether this is a user or platform principal.
    pub const fn principal_type(&self) -> PrincipalType {
        self.type_
    }

    /// Return the normalized first-party subject.
    pub fn subject(&self) -> &str {
        &self.subject
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
/// use wamn_identity::AuthenticatedPrincipal;
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
    /// The configured envelope backend refused signing-key encryption or decryption.
    Encryption,
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
    source: Option<wamn_envelope::encryption::Error>,
}

impl IdentityError {
    pub fn new(type_: IdentityErrorType, message: impl Into<Box<str>>) -> Self {
        Self {
            type_,
            message: message.into(),
            refusal: None,
            source: None,
        }
    }

    /// Create an error with the refusal reason that a route answers to its caller.
    pub fn refused(
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

impl std::error::Error for IdentityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source as &dyn std::error::Error)
    }
}

impl From<wamn_envelope::encryption::Error> for IdentityError {
    fn from(source: wamn_envelope::encryption::Error) -> Self {
        Self {
            source: Some(source),
            ..Self::new(
                IdentityErrorType::Encryption,
                "session signing key envelope operation failed",
            )
        }
    }
}

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
    client: &mut sqlx::PgConnection,
    subject: &str,
    email: &str,
    display_name: &str,
) -> Result<Principal, IdentityError> {
    let subject = canonical_subject(subject)?;
    let email = require_email(email)?;
    let display_name = require_display_name(display_name)?;
    let row = sqlx::query(INSERT_USER_SQL)
        .bind(&subject)
        .bind(&email)
        .bind(&display_name)
        .fetch_one(&mut *client)
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
    client: &mut sqlx::PgConnection,
    email: &str,
    display_name: &str,
) -> Result<UserAccount, IdentityError> {
    let email = require_email(email)?;
    let subject = canonical_subject(&email)?;
    let display_name = require_display_name(display_name)?;
    sqlx::query(INSERT_USER_IF_ABSENT_SQL)
        .bind(&subject)
        .bind(&email)
        .bind(&display_name)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    let row = sqlx::query(SELECT_USER_BY_EMAIL_SQL)
        .bind(&email)
        .fetch_optional(&mut *client)
        .await
        .map_err(|error| database_error(&error))?
        .ok_or_else(|| {
            IdentityError::new(
                IdentityErrorType::Conflict,
                "the subject of this email belongs to another principal",
            )
        })?;
    let principal_id: PrincipalId = row.try_get(0).map_err(|error| database_error(&error))?;
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

/// Resolve a principal by its opaque ID without authenticating the caller.
pub async fn resolve_principal(
    client: &mut sqlx::PgConnection,
    id: &PrincipalId,
) -> Result<Option<Principal>, IdentityError> {
    sqlx::query(SELECT_PRINCIPAL_BY_ID_SQL)
        .bind(id)
        .fetch_optional(&mut *client)
        .await
        .map_err(|error| database_error(&error))?
        .as_ref()
        .map(decode_principal)
        .transpose()
}

/// Disable a principal. Repeated calls are idempotent and retain identity data.
pub async fn disable_principal(
    client: &mut sqlx::PgConnection,
    id: &PrincipalId,
) -> Result<Principal, IdentityError> {
    let row = sqlx::query(DISABLE_PRINCIPAL_SQL)
        .bind(id)
        .fetch_optional(&mut *client)
        .await
        .map_err(|error| database_error(&error))?
        .ok_or_else(|| {
            IdentityError::new(IdentityErrorType::NotFound, "principal does not exist")
        })?;
    decode_principal(&row)
}

/// Assign one opaque role slug to a principal in a registered project.
pub async fn assign_project_role(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    role: &str,
) -> Result<(), IdentityError> {
    org::active_member(client, principal_id, org).await?;
    let role = role.parse::<RoleName>()?.to_string();
    sqlx::query(ASSIGN_PROJECT_ROLE_SQL)
        .bind(principal_id)
        .bind(org)
        .bind(project)
        .bind(&role)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    Ok(())
}

/// Read a principal's role slugs in stable lexical order for one project.
pub async fn project_roles(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
) -> Result<Vec<RoleName>, IdentityError> {
    let rows = sqlx::query(SELECT_PROJECT_ROLES_SQL)
        .bind(principal_id)
        .bind(org)
        .bind(project)
        .fetch_all(&mut *client)
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
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
) -> Result<(), IdentityError> {
    org::active_member(client, principal_id, org).await?;
    sqlx::query(GRANT_PROJECT_ENV_MEMBERSHIP_SQL)
        .bind(principal_id)
        .bind(org)
        .bind(project)
        .bind(env)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    Ok(())
}

/// Read the explicit membership fact for one principal and project-environment.
///
/// Credential authentication must separately verify that the principal is active.
/// Project management roles and memberships in other environments do not count.
pub async fn has_project_env_membership(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
) -> Result<bool, IdentityError> {
    sqlx::query(SELECT_PROJECT_ENV_MEMBERSHIP_SQL)
        .bind(principal_id)
        .bind(org)
        .bind(project)
        .bind(env)
        .fetch_one(&mut *client)
        .await
        .map_err(|error| database_error(&error))?
        .try_get(0)
        .map_err(|error| database_error(&error))
}

/// Revoke membership, returning whether an explicit grant was removed.
pub async fn revoke_project_env_membership(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
) -> Result<bool, IdentityError> {
    org::lock_membership(client, principal_id, org).await?;
    let removed = sqlx::query(REVOKE_PROJECT_ENV_MEMBERSHIP_SQL)
        .bind(principal_id)
        .bind(org)
        .bind(project)
        .bind(env)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    Ok(removed != 0)
}

fn decode_principal(row: &Row) -> Result<Principal, IdentityError> {
    let id: PrincipalId = row.try_get(0).map_err(|error| database_error(&error))?;
    let type_: String = row.try_get(1).map_err(|error| database_error(&error))?;
    let subject: String = row.try_get(2).map_err(|error| database_error(&error))?;
    let display_name: String = row.try_get(3).map_err(|error| database_error(&error))?;
    let status: String = row.try_get(4).map_err(|error| database_error(&error))?;
    Ok(Principal {
        id,
        type_: PrincipalType::parse(&type_)?,
        subject: subject.into(),
        display_name: display_name.into(),
        status: PrincipalStatus::parse(&status)?,
    })
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

pub fn database_error(error: &sqlx::Error) -> IdentityError {
    let type_ = match error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .as_deref()
    {
        Some("23505") => IdentityErrorType::Conflict,
        Some("23503") => IdentityErrorType::NotFound,
        _ => IdentityErrorType::Database,
    };
    // `sqlx::Error` displays as the bare string "db error", so the
    // readable half is taken from its database error. That value's own Display
    // carries PostgreSQL's DETAIL, which repeats the offending key, so the
    // message and the constraint name are read directly instead. An operator
    // learns which rule refused the write, and the row's data stays out of the
    // log.
    let reason = error.as_database_error().map_or_else(
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
    fn principal_type_literals_round_trip() {
        for type_ in [
            PrincipalType::User,
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
}
