//! Postgres application administration under the caller's transaction.

use sqlx::{PgConnection, Row as _};
use uuid::Uuid;
use wamn_authentication::administration::{
    self, AdministrationError, ApplicationTarget, ApplicationUserRow, GrantAuthority, GrantChange,
    GrantError, GrantOutcome, GrantStore, PermissionGrantOutcome,
};
use wamn_authentication::identity_error::{IdentityError, IdentityErrorType};
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
    (status) VALUES ($1) \
    ON CONFLICT (id) DO UPDATE SET status = EXCLUDED.status \
    WHERE platform.environment.status <> EXCLUDED.status";

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
        .bind(status.as_str())
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

const USER_EXISTS_SQL: &str = "SELECT 1 FROM platform.\"user\" \
    WHERE id = $1 AND type IN ('user', 'service')";

const USER_LIST_SQL: &str = "SELECT u.id, u.email, u.display_name, \
    COALESCE(array_agg(r.role_name ORDER BY r.role_name) \
             FILTER (WHERE r.role_name IS NOT NULL), '{}') \
    FROM platform.\"user\" u \
    LEFT JOIN platform.user_role r ON r.user_id = u.id \
    WHERE u.type = 'user' \
    GROUP BY u.id, u.email, u.display_name ORDER BY u.email, u.id";

const ROLE_LIST_SQL: &str = "SELECT name FROM platform.role ORDER BY name";

const PERMISSION_LIST_SQL: &str = "SELECT permission FROM platform.permission \
    WHERE role_name = $1 ORDER BY permission";

/// The application users of the database with their roles, by email.
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

// The transaction holds the operation grant lock and binds the actor.
struct GrantTransaction<'a>(&'a mut PgConnection);

#[async_trait::async_trait]
impl GrantStore for GrantTransaction<'_> {
    async fn insert_role(&mut self, role: &str) -> Result<bool, IdentityError> {
        insert_role(self.0, role).await
    }

    async fn remove_role(&mut self, role: &str) -> Result<bool, IdentityError> {
        let removed = sqlx::query(DROP_ROLE_SQL)
            .bind(role)
            .execute(&mut *self.0)
            .await
            .map_err(|error| database_error(&error))?;
        Ok(removed.rows_affected() == 1)
    }

    async fn role_exists(&mut self, role: &str) -> Result<bool, IdentityError> {
        Ok(sqlx::query(ROLE_EXISTS_SQL)
            .bind(role)
            .fetch_optional(&mut *self.0)
            .await
            .map_err(|error| database_error(&error))?
            .is_some())
    }

    async fn user_exists(&mut self, user_id: &PrincipalId) -> Result<bool, IdentityError> {
        Ok(sqlx::query(USER_EXISTS_SQL)
            .bind(Uuid::from(*user_id))
            .fetch_optional(&mut *self.0)
            .await
            .map_err(|error| database_error(&error))?
            .is_some())
    }

    async fn is_admin(&mut self, actor: &PrincipalId) -> Result<bool, IdentityError> {
        Ok(sqlx::query(
            "SELECT EXISTS (SELECT 1 FROM platform.user_role WHERE user_id=$1 AND role_name='admin')",
        )
        .bind(Uuid::from(*actor))
        .fetch_one(&mut *self.0)
        .await
        .map_err(|error| database_error(&error))?
        .get(0))
    }

    async fn holds_permission(
        &mut self,
        actor: &PrincipalId,
        reference: &str,
    ) -> Result<bool, IdentityError> {
        Ok(sqlx::query(
            "SELECT EXISTS (SELECT 1 FROM platform.user_role r JOIN platform.permission p ON p.role_name=r.role_name \
             WHERE r.user_id=$1 AND p.permission=$2)",
        )
        .bind(Uuid::from(*actor))
        .bind(reference)
        .fetch_one(&mut *self.0)
        .await
        .map_err(|error| database_error(&error))?
        .get(0))
    }

    async fn permissions(&mut self, role: &str) -> Result<Vec<String>, IdentityError> {
        Ok(sqlx::query(PERMISSION_LIST_SQL)
            .bind(role)
            .fetch_all(&mut *self.0)
            .await
            .map_err(|error| database_error(&error))?
            .iter()
            .map(|row| row.get(0))
            .collect())
    }

    async fn insert_permission(
        &mut self,
        role: &str,
        reference: &str,
    ) -> Result<PermissionGrantOutcome, IdentityError> {
        let result = sqlx::query(PERMISSION_SQL)
            .bind(role)
            .bind(reference)
            .execute(&mut *self.0)
            .await
            .map_err(|error| database_error(&error))?;
        Ok(PermissionGrantOutcome {
            rows_added: result.rows_affected(),
        })
    }

    async fn remove_permission(
        &mut self,
        role: &str,
        reference: &str,
    ) -> Result<bool, IdentityError> {
        let result = sqlx::query(DROP_PERMISSION_SQL)
            .bind(role)
            .bind(reference)
            .execute(&mut *self.0)
            .await
            .map_err(|error| database_error(&error))?;
        Ok(result.rows_affected() != 0)
    }

    async fn assign_role(
        &mut self,
        user_id: &PrincipalId,
        role: &str,
    ) -> Result<bool, IdentityError> {
        assign_role(self.0, user_id, role).await
    }

    async fn unassign_role(
        &mut self,
        user_id: &PrincipalId,
        role: &str,
    ) -> Result<bool, IdentityError> {
        unassign_role(self.0, user_id, role).await
    }
}

async fn insert_role(client: &mut PgConnection, role: &str) -> Result<bool, IdentityError> {
    let written = sqlx::query(ROLE_SQL)
        .bind(role)
        .execute(client)
        .await
        .map_err(|error| database_error(&error))?;
    Ok(written.rows_affected() == 1)
}

/// Give the existing role `role` to the user `user_id`. A second grant
/// changes nothing.
async fn assign_role(
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
async fn unassign_role(
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

/// Create an authored role in the caller's transaction.
pub async fn create_role(
    client: &mut PgConnection,
    role: &str,
) -> Result<bool, AdministrationError> {
    administration::create_role(&mut GrantTransaction(client), role).await
}

/// Delete an authored role in the caller's transaction.
pub async fn delete_role(
    client: &mut PgConnection,
    role: &str,
) -> Result<bool, AdministrationError> {
    administration::delete_role(&mut GrantTransaction(client), role).await
}

/// Read the stored permissions held by an existing role.
pub async fn permission_rows(
    client: &mut PgConnection,
    role: &str,
) -> Result<Vec<String>, AdministrationError> {
    administration::permission_rows(&mut GrantTransaction(client), role).await
}

/// Mutate an authored grant inside the caller's prepared transaction.
pub async fn mutate_grant(
    client: &mut PgConnection,
    project_env_key: &wamn_registry::ProjectEnvKey,
    authority: GrantAuthority<'_>,
    change: GrantChange<'_>,
) -> Result<GrantOutcome, GrantError> {
    administration::mutate_grant(
        &mut GrantTransaction(client),
        project_env_key,
        authority,
        change,
    )
    .await
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
        "SELECT p.type, p.status, p.email, p.display_name, \
         EXISTS (SELECT 1 FROM identity.org_memberships m WHERE m.principal_id=p.id AND m.org=$2 AND m.status='active'), \
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
    let active = row.get::<String, _>(1) == "active";
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
