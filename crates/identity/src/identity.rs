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

use crate::principal::{Principal, canonical_subject, require_display_name, require_email};
use sqlx::Row as _;
use sqlx::postgres::PgRow as Row;
use sqlx::types::Uuid;
use wamn_authentication::identity_error::{IdentityError, IdentityErrorType};
use wamn_identity_protocol::{PrincipalId, records::UserAccount};
use wamn_role::RoleName;

use wamn_authentication::identity_error::database_error;

pub mod application;
pub mod control;
pub mod issuer;
pub mod nats;
mod nats_store;
pub mod org;
pub mod password;
pub mod password_login;
pub mod session_keys;
pub mod session_token;
pub mod transaction;

const INSERT_USER_SQL: &str = "INSERT INTO identity.principals \
    (type, subject, email, display_name) VALUES ('user', $1, $2, $3) \
    RETURNING id, type, subject, display_name, active";
/// The user insert of [`create_or_reuse_user`]. An existing email or subject
/// skips it.
const INSERT_USER_IF_ABSENT_SQL: &str = "INSERT INTO identity.principals \
    (type, subject, email, display_name) VALUES ('user', $1, $2, $3) \
    ON CONFLICT DO NOTHING";
const SELECT_USER_BY_EMAIL_SQL: &str = "SELECT p.id, p.active, EXISTS (\
    SELECT 1 FROM identity.password_credentials c WHERE c.principal_id = p.id) \
    FROM identity.principals p WHERE p.type = 'user' AND p.email = $1";
const SELECT_PRINCIPAL_BY_ID_SQL: &str = "SELECT id, type, subject, \
    display_name, active FROM identity.principals WHERE id = $1";
const DISABLE_PRINCIPAL_SQL: &str = "UPDATE identity.principals \
    SET active = false \
    WHERE id = $1 \
    RETURNING id, type, subject, display_name, active";
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
    let principal_id = PrincipalId::from(
        row.try_get::<Uuid, _>(0)
            .map_err(|error| database_error(&error))?,
    );
    let active: bool = row.try_get(1).map_err(|error| database_error(&error))?;
    if !active {
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
        .bind(Uuid::from(*id))
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
        .bind(Uuid::from(*id))
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
    write_project_role(client, principal_id, org, project, &role).await
}

async fn write_project_role(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    role: &str,
) -> Result<(), IdentityError> {
    sqlx::query(ASSIGN_PROJECT_ROLE_SQL)
        .bind(Uuid::from(*principal_id))
        .bind(org.as_str())
        .bind(project.as_str())
        .bind(role)
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
        .bind(Uuid::from(*principal_id))
        .bind(org.as_str())
        .bind(project.as_str())
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
        .bind(Uuid::from(*principal_id))
        .bind(org.as_str())
        .bind(project.as_str())
        .bind(env.as_str())
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
        .bind(Uuid::from(*principal_id))
        .bind(org.as_str())
        .bind(project.as_str())
        .bind(env.as_str())
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
        .bind(Uuid::from(*principal_id))
        .bind(org.as_str())
        .bind(project.as_str())
        .bind(env.as_str())
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    Ok(removed != 0)
}

fn decode_principal(row: &Row) -> Result<Principal, IdentityError> {
    let id = PrincipalId::from(
        row.try_get::<Uuid, _>(0)
            .map_err(|error| database_error(&error))?,
    );
    let type_: String = row.try_get(1).map_err(|error| database_error(&error))?;
    let subject: String = row.try_get(2).map_err(|error| database_error(&error))?;
    let display_name: String = row.try_get(3).map_err(|error| database_error(&error))?;
    let active: bool = row.try_get(4).map_err(|error| database_error(&error))?;
    Principal::from_storage(id, &type_, subject, display_name, active)
}
