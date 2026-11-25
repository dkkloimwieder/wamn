//! Postgres application administration under the caller's transaction.

use super::{AdministrationError, ApplicationTarget, ApplicationUserRow, PermissionGrantOutcome};
use crate::identity_error::{IdentityError, IdentityErrorType};
use sqlx::{PgConnection, Row as _};
use uuid::Uuid;
use wamn_identity_protocol::{ADMIN_ROLE, PrincipalId};

use crate::identity_error::database_error;

/// Whether the project database has its schema's platform seed rows.
const PLATFORM_ROWS_SQL: &str =
    "SELECT EXISTS (SELECT FROM platform.\"user\" WHERE type = 'platform')";

const ROLE_SQL: &str = "INSERT INTO platform.role (name) VALUES ($1) \
    ON CONFLICT (name) DO NOTHING";

const USER_ROLE_SQL: &str = "INSERT INTO platform.user_role (user_id, role_name) \
    VALUES ($1, $2) ON CONFLICT DO NOTHING";

const DROP_USER_ROLE_SQL: &str = "DELETE FROM platform.user_role \
    WHERE user_id = $1 AND role_name = $2";

/// The status row of the environment. No row means active.
const ENVIRONMENT_STATUS_ROW_SQL: &str = "INSERT INTO platform.environment \
    (active) VALUES ($1) \
    ON CONFLICT (id) DO UPDATE SET active = EXCLUDED.active \
    WHERE platform.environment.active <> EXCLUDED.active";

/// Refuse a project database without platform rows. The database is the org
/// boundary, so its rows name no project: the caller derives the project of
/// the environment from its triple (`project_env_key`).
pub async fn require_platform_rows(client: &mut sqlx::PgConnection) -> Result<(), IdentityError> {
    let present: bool = sqlx::query(PLATFORM_ROWS_SQL)
        .fetch_one(&mut *client)
        .await
        .map_err(|error| database_error(&error))?
        .get(0);
    if present {
        Ok(())
    } else {
        Err(IdentityError::new(
            IdentityErrorType::NotFound,
            "the project database has no platform rows. Run `wamn env apply` first",
        ))
    }
}

/// Write the status row of the environment, the mirror of its system row that
/// its host reads (docs/architecture/execution.md#routing-and-availability).
pub async fn write_environment_status(
    client: &mut sqlx::PgConnection,
    status: wamn_registry::EnvironmentStatus,
) -> Result<(), IdentityError> {
    sqlx::query(ENVIRONMENT_STATUS_ROW_SQL)
        .bind(status == wamn_registry::EnvironmentStatus::Active)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    Ok(())
}

const ROLE_EXISTS_SQL: &str = "SELECT 1 FROM platform.role WHERE name = $1";

const DROP_ROLE_SQL: &str = "DELETE FROM platform.role WHERE name = $1";

const PERMISSION_SQL: &str = "INSERT INTO platform.permission \
    (role_name, permission) VALUES ($1, $2) \
    ON CONFLICT DO NOTHING";

const DROP_PERMISSION_SQL: &str = "DELETE FROM platform.permission \
    WHERE role_name = $1 AND permission = $2";

const LOCK_GRANT_RECIPIENT_SQL: &str = "SELECT 1 FROM platform.\"user\" \
    WHERE id = $1 AND (type = 'service' OR (type = 'user' AND active)) FOR SHARE";

const USER_LIST_SQL: &str = "SELECT u.id, u.email, u.display_name, \
    COALESCE(array_agg(r.role_name ORDER BY r.role_name) \
             FILTER (WHERE r.role_name IS NOT NULL), '{}') \
    FROM platform.\"user\" u \
    LEFT JOIN platform.user_role r ON r.user_id = u.id \
    WHERE u.type = 'user' AND u.active \
    GROUP BY u.id, u.email, u.display_name ORDER BY u.email, u.id";

const ROLE_LIST_SQL: &str = "SELECT name FROM platform.role ORDER BY name";

const PERMISSION_LIST_SQL: &str = "SELECT permission FROM platform.permission \
    WHERE role_name = $1 ORDER BY permission";

/// The application users eligible to hold roles, by email.
pub async fn application_users(
    client: &mut sqlx::PgConnection,
) -> Result<Vec<ApplicationUserRow>, AdministrationError> {
    sqlx::query(USER_LIST_SQL)
        .fetch_all(&mut *client)
        .await
        .map_err(|error| database_error(&error))?
        .iter()
        .map(|row| {
            Ok(ApplicationUserRow {
                id: PrincipalId::from(
                    row.try_get::<Uuid, _>(0)
                        .map_err(|error| database_error(&error))?,
                ),
                email: row.get(1),
                display_name: row.get(2),
                roles: row.get(3),
            })
        })
        .collect()
}

/// The role names of the database, `admin` included.
pub async fn role_names(
    client: &mut sqlx::PgConnection,
) -> Result<Vec<String>, AdministrationError> {
    Ok(sqlx::query(ROLE_LIST_SQL)
        .fetch_all(&mut *client)
        .await
        .map_err(|error| database_error(&error))?
        .iter()
        .map(|row| row.get(0))
        .collect())
}

pub(super) async fn remove_role(
    client: &mut PgConnection,
    role: &str,
) -> Result<bool, IdentityError> {
    let removed = sqlx::query(DROP_ROLE_SQL)
        .bind(role)
        .execute(client)
        .await
        .map_err(|error| database_error(&error))?;
    Ok(removed.rows_affected() == 1)
}

pub(super) async fn role_exists(
    client: &mut PgConnection,
    role: &str,
) -> Result<bool, IdentityError> {
    Ok(sqlx::query(ROLE_EXISTS_SQL)
        .bind(role)
        .fetch_optional(client)
        .await
        .map_err(|error| database_error(&error))?
        .is_some())
}

pub(super) async fn lock_grant_recipient(
    client: &mut PgConnection,
    user_id: &PrincipalId,
) -> Result<bool, IdentityError> {
    Ok(sqlx::query(LOCK_GRANT_RECIPIENT_SQL)
        .bind(Uuid::from(*user_id))
        .fetch_optional(client)
        .await
        .map_err(|error| database_error(&error))?
        .is_some())
}

pub(super) async fn is_admin(
    client: &mut PgConnection,
    actor: &PrincipalId,
) -> Result<bool, IdentityError> {
    Ok(sqlx::query(
        "SELECT EXISTS (SELECT 1 FROM platform.user_role WHERE user_id=$1 AND role_name='admin')",
    )
    .bind(Uuid::from(*actor))
    .fetch_one(client)
    .await
    .map_err(|error| database_error(&error))?
    .get(0))
}

pub(super) async fn holds_permission(
    client: &mut PgConnection,
    actor: &PrincipalId,
    reference: &str,
) -> Result<bool, IdentityError> {
    Ok(sqlx::query(
        "SELECT EXISTS (SELECT 1 FROM platform.user_role r JOIN platform.permission p ON p.role_name=r.role_name \
         WHERE r.user_id=$1 AND p.permission=$2)",
    )
    .bind(Uuid::from(*actor))
    .bind(reference)
    .fetch_one(client)
    .await
    .map_err(|error| database_error(&error))?
    .get(0))
}

pub(super) async fn permissions(
    client: &mut PgConnection,
    role: &str,
) -> Result<Vec<String>, IdentityError> {
    Ok(sqlx::query(PERMISSION_LIST_SQL)
        .bind(role)
        .fetch_all(client)
        .await
        .map_err(|error| database_error(&error))?
        .iter()
        .map(|row| row.get(0))
        .collect())
}

pub(super) async fn insert_permission(
    client: &mut PgConnection,
    role: &str,
    reference: &str,
) -> Result<PermissionGrantOutcome, IdentityError> {
    let result = sqlx::query(PERMISSION_SQL)
        .bind(role)
        .bind(reference)
        .execute(client)
        .await
        .map_err(|error| database_error(&error))?;
    Ok(PermissionGrantOutcome {
        rows_added: result.rows_affected(),
    })
}

pub(super) async fn remove_permission(
    client: &mut PgConnection,
    role: &str,
    reference: &str,
) -> Result<bool, IdentityError> {
    let result = sqlx::query(DROP_PERMISSION_SQL)
        .bind(role)
        .bind(reference)
        .execute(client)
        .await
        .map_err(|error| database_error(&error))?;
    Ok(result.rows_affected() != 0)
}

pub(super) async fn insert_role(
    client: &mut PgConnection,
    role: &str,
) -> Result<bool, IdentityError> {
    let written = sqlx::query(ROLE_SQL)
        .bind(role)
        .execute(client)
        .await
        .map_err(|error| database_error(&error))?;
    Ok(written.rows_affected() == 1)
}

/// Give the existing role `role` to the user `user_id`. A second grant
/// changes nothing.
pub(super) async fn assign_role(
    client: &mut sqlx::PgConnection,
    user_id: &wamn_identity_protocol::PrincipalId,
    role: &str,
) -> Result<bool, IdentityError> {
    let written = sqlx::query(USER_ROLE_SQL)
        .bind(Uuid::from(*user_id))
        .bind(role)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    Ok(written == 1)
}

/// Take the role `role` from the user `user_id`. A revoke of a role the user
/// does not hold changes nothing.
pub(super) async fn unassign_role(
    client: &mut sqlx::PgConnection,
    user_id: &wamn_identity_protocol::PrincipalId,
    role: &str,
) -> Result<bool, IdentityError> {
    let removed = sqlx::query(DROP_USER_ROLE_SQL)
        .bind(Uuid::from(*user_id))
        .bind(role)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    Ok(removed == 1)
}

/// System authority used to project an active user into a project database.
#[derive(Debug)]
pub struct ProjectAuthority {
    pub email: String,
    pub display_name: String,
    pub admin: bool,
}

/// Read a user's authority after the caller locks membership.
pub async fn read_project_authority(
    system: &mut PgConnection,
    coordinate: ApplicationTarget<'_>,
    principal: &PrincipalId,
) -> Result<Option<ProjectAuthority>, IdentityError> {
    let row = sqlx::query(
        "SELECT p.type, p.active, p.email, p.display_name, \
         EXISTS (SELECT 1 FROM identity.org_memberships m WHERE m.principal_id=p.id AND m.org=$2 AND m.active), \
         identity.has_project_access(p.id,$2,$4,$3), \
         identity.is_project_admin(p.id,$2,$3) \
         FROM identity.principals p WHERE p.id=$1",
    )
    .bind(Uuid::from(*principal))
    .bind(coordinate.org.as_str())
    .bind(coordinate.project.as_str())
    .bind(coordinate.env.as_str())
    .fetch_optional(system)
    .await
    .map_err(|error| database_error(&error))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let type_: String = row.get(0);
    let active = row.get::<bool, _>(1);
    if type_ != "user" || !active || !row.get::<bool, _>(4) || !row.get::<bool, _>(5) {
        return Ok(None);
    }
    Ok(Some(ProjectAuthority {
        email: row.get(2),
        display_name: row.get(3),
        admin: row.get(6),
    }))
}

/// Mirror the system administrator decision in the project transaction.
pub async fn project_administration(
    target: &mut PgConnection,
    principal: &PrincipalId,
    admin: bool,
) -> Result<(), IdentityError> {
    if admin {
        insert_role(target, ADMIN_ROLE).await?;
        assign_role(target, principal, ADMIN_ROLE).await?;
    } else {
        unassign_role(target, principal, ADMIN_ROLE).await?;
    }
    Ok(())
}
