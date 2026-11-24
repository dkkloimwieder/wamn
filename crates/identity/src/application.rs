//! Project database identities and authored application grants
//! (docs/architecture/overview.md#organization-membership-and-administration).
//!
//! System writers commit authority before projection. Projection reads current
//! authority under the org membership lock and commits application data before
//! releasing that lock. Authored grants use `mutate_grant` inside the application
//! transaction. The CLI calls the HTTP routes.

pub mod service;

mod user;
pub use user::{ApplicationUser, disable_connection_services, remove_user, write_user};

use sqlx::Row as _;
use sqlx::types::Uuid;
use std::fmt;

use crate::{IdentityError, IdentityErrorType, database_error};
use wamn_identity_protocol::{ADMIN_ROLE, PrincipalId};

/// Serialize the writes of the environment's roles and permission rows.
///
/// Package lineage remains independently locked per family. Role and
/// permission writes additionally share this environment lock.
pub const OPERATION_GRANT_LOCK_SQL: &str = "SELECT pg_advisory_xact_lock(hashtextextended(\
     'wamn.operation-grants:' || $1, 0))";

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

/// Why an application administration write refused
/// (docs/architecture/execution.md#host-run-routes). Each refusal changes no
/// row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdministrationRefusal {
    /// `admin` holds every operation the release serves.
    AdminTakesNoPermission,
    /// `apply-package` creates `admin`, so no caller creates it.
    AdminCreated,
    /// `admin` is the built-in role.
    AdminDeleted,
    /// The name is not a role slug.
    NotProjectRole { role: String },
    /// The role does not exist in the environment.
    RoleNotFound { role: String },
    /// The operation is fixed to `admin` or to every member (§2.5).
    NotGrantable { reference: String },
    /// The environment has no application user with the id.
    UserNotFound {
        user_id: wamn_identity_protocol::PrincipalId,
    },
    /// `admin` stays while the user holds `project-admin` in the project,
    /// including administration inherited from its org or environment.
    AdminCovered {
        user_id: wamn_identity_protocol::PrincipalId,
    },
    /// The role does not hold the operation.
    NotHeld { role: String, reference: String },
}

impl fmt::Display for AdministrationRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AdminTakesNoPermission => formatter.write_str(
                "admin holds every operation the release serves, so it takes no permission",
            ),
            Self::AdminCreated => {
                formatter.write_str("admin is the built-in role; apply-package creates it")
            }
            Self::AdminDeleted => {
                formatter.write_str("admin is the built-in role and cannot be deleted")
            }
            Self::NotProjectRole { role } => write!(
                formatter,
                "role {role:?} is not a role name: lowercase letters, digits and hyphens after \
                 the first character, at most 64 bytes"
            ),
            Self::RoleNotFound { role } => {
                write!(formatter, "role {role} does not exist in the environment")
            }
            Self::NotGrantable { reference } => write!(
                formatter,
                "{reference} is fixed to admin or to every member, so no role takes it"
            ),
            Self::UserNotFound { user_id } => {
                write!(formatter, "the application has no user {user_id}")
            }
            Self::AdminCovered { user_id } => write!(
                formatter,
                "user {user_id} holds project-admin in the project, so admin stays. \
                 Revoke project-admin or org-admin first"
            ),
            Self::NotHeld { role, reference } => {
                write!(formatter, "role {role} does not hold {reference}")
            }
        }
    }
}

/// A refused or failed application administration write.
#[derive(Debug)]
pub enum AdministrationError {
    Refused(AdministrationRefusal),
    Failed(IdentityError),
}

impl fmt::Display for AdministrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(refusal) => refusal.fmt(formatter),
            Self::Failed(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for AdministrationError {}

impl From<IdentityError> for AdministrationError {
    fn from(error: IdentityError) -> Self {
        Self::Failed(error)
    }
}

impl From<AdministrationRefusal> for AdministrationError {
    fn from(refusal: AdministrationRefusal) -> Self {
        Self::Refused(refusal)
    }
}

/// What a permission grant changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionGrantOutcome {
    /// Rows written: the permission, or none on a second grant.
    pub rows_added: u64,
}

const ROLE_EXISTS_SQL: &str = "SELECT 1 FROM platform.role WHERE name = $1";

const DROP_ROLE_SQL: &str = "DELETE FROM platform.role WHERE name = $1";

const PERMISSION_SQL: &str = "INSERT INTO platform.permission \
    (role_name, permission) VALUES ($1, $2) \
    ON CONFLICT DO NOTHING";

const DROP_PERMISSION_SQL: &str = "DELETE FROM platform.permission \
    WHERE role_name = $1 AND permission = $2";

// The writes below run in a transaction that the caller prepared: it holds
// the project's operation grant lock and binds the actor of the writes.
// `wamn` binds `wamn:provisioning`, and a host route binds its caller.

/// Create the empty authored role `role`. A second create changes nothing.
pub async fn create_role(
    client: &mut sqlx::PgConnection,
    role: &str,
) -> Result<bool, AdministrationError> {
    if role == ADMIN_ROLE {
        return Err(AdministrationRefusal::AdminCreated.into());
    }
    if !role
        .parse::<wamn_role::RoleName>()
        .is_ok_and(|parsed| parsed.as_str() == role)
    {
        return Err(AdministrationRefusal::NotProjectRole {
            role: role.to_owned(),
        }
        .into());
    }
    let written = sqlx::query(ROLE_SQL)
        .bind(role)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    Ok(written == 1)
}

/// Delete the authored role `role` with its assignments and permissions. A
/// delete of an absent role changes nothing.
pub async fn delete_role(
    client: &mut sqlx::PgConnection,
    role: &str,
) -> Result<bool, AdministrationError> {
    if role == ADMIN_ROLE {
        return Err(AdministrationRefusal::AdminDeleted.into());
    }
    let removed = sqlx::query(DROP_ROLE_SQL)
        .bind(role)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    Ok(removed == 1)
}

/// Refuse unless `role` exists in the environment.
pub async fn require_role(
    client: &mut sqlx::PgConnection,
    role: &str,
) -> Result<(), AdministrationError> {
    let exists = sqlx::query(ROLE_EXISTS_SQL)
        .bind(role)
        .fetch_optional(&mut *client)
        .await
        .map_err(|error| database_error(&error))?
        .is_some();
    if exists {
        Ok(())
    } else {
        Err(AdministrationRefusal::RoleNotFound {
            role: role.to_owned(),
        }
        .into())
    }
}

/// Grant `reference` to the authored `role`: store it as a row. The grant
/// stores any reference that `permission_reference_check` accepts, whether a
/// release serves it or not.
async fn grant_permission(
    client: &mut sqlx::PgConnection,
    role: &str,
    reference: &str,
) -> Result<PermissionGrantOutcome, AdministrationError> {
    let rows_added = sqlx::query(PERMISSION_SQL)
        .bind(role)
        .bind(reference)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    Ok(PermissionGrantOutcome { rows_added })
}

/// Remove the row `reference` from `role`. A revoke of a reference the role
/// does not hold refuses.
async fn revoke_permission(
    client: &mut sqlx::PgConnection,
    role: &str,
    reference: &str,
) -> Result<(), AdministrationError> {
    let removed = sqlx::query(DROP_PERMISSION_SQL)
        .bind(role)
        .bind(reference)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    if removed == 0 {
        return Err(AdministrationRefusal::NotHeld {
            role: role.to_owned(),
            reference: reference.to_owned(),
        }
        .into());
    }
    Ok(())
}

/// Give the existing role `role` to the user `user_id`. A second grant
/// changes nothing.
async fn grant_user_role(
    client: &mut sqlx::PgConnection,
    user_id: &wamn_identity_protocol::PrincipalId,
    role: &str,
) -> Result<bool, AdministrationError> {
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
async fn revoke_user_role(
    client: &mut sqlx::PgConnection,
    user_id: &wamn_identity_protocol::PrincipalId,
    role: &str,
) -> Result<bool, AdministrationError> {
    let removed = sqlx::query(DROP_USER_ROLE_SQL)
        .bind(Uuid::from(*user_id))
        .bind(role)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    Ok(removed == 1)
}

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

/// One application user with the roles it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationUserRow {
    pub id: PrincipalId,
    pub email: String,
    pub display_name: Option<String>,
    pub roles: Vec<String>,
}

/// Refuse unless the database has the application user `user_id`.
pub async fn require_user(
    client: &mut sqlx::PgConnection,
    user_id: &wamn_identity_protocol::PrincipalId,
) -> Result<(), AdministrationError> {
    let exists = sqlx::query(USER_EXISTS_SQL)
        .bind(Uuid::from(*user_id))
        .fetch_optional(&mut *client)
        .await
        .map_err(|error| database_error(&error))?
        .is_some();
    if exists {
        Ok(())
    } else {
        Err(AdministrationRefusal::UserNotFound {
            user_id: user_id.to_owned(),
        }
        .into())
    }
}

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

/// The stored permissions of `role`.
pub async fn permission_rows(
    client: &mut sqlx::PgConnection,
    role: &str,
) -> Result<Vec<String>, AdministrationError> {
    require_role(client, role).await?;
    Ok(sqlx::query(PERMISSION_LIST_SQL)
        .bind(role)
        .fetch_all(&mut *client)
        .await
        .map_err(|error| database_error(&error))?
        .iter()
        .map(|row| row.get(0))
        .collect())
}

/// Rebuild one principal's application authority from a locked system transaction.
/// The caller locks membership before opening the application transaction and
/// commits the application transaction before ending the system transaction.
pub async fn project_authority(
    system: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    target: &mut sqlx::PgConnection,
    coordinate: ApplicationTarget<'_>,
    principal: &PrincipalId,
    issuer: &str,
) -> Result<(), IdentityError> {
    crate::org::lock_membership(system, principal, coordinate.org).await?;
    let row = sqlx::query("SELECT p.type, p.status, p.email, p.display_name, \
         EXISTS (SELECT 1 FROM identity.org_memberships m WHERE m.principal_id=p.id AND m.org=$2 AND m.status='active'), \
         identity.has_project_access(p.id,$2,$4,$3), \
         identity.is_project_admin(p.id,$2,$3) \
         FROM identity.principals p WHERE p.id=$1").bind(Uuid::from(*principal)).bind(coordinate.org.as_str()).bind(coordinate.project.as_str()).bind(coordinate.env.as_str()).fetch_optional(&mut **system).await.map_err(|error| database_error(&error))?;
    let Some(row) = row else {
        return remove_user(target, principal).await;
    };
    let type_: String = row.get(0);
    let active = row.get::<String, _>(1) == "active";
    if type_ != "user" || !active || !row.get::<bool, _>(4) || !row.get::<bool, _>(5) {
        return remove_user(target, principal).await;
    }
    let email: String = row.get(2);
    let display_name: String = row.get(3);
    let user = ApplicationUser {
        principal_id: principal,
        email: &email,
        display_name: &display_name,
    };
    write_user(target, issuer, user).await?;
    let admin: bool = row.get(6);
    if !admin {
        return sqlx::query(DROP_USER_ROLE_SQL)
            .bind(Uuid::from(*principal))
            .bind(ADMIN_ROLE)
            .execute(&mut *target)
            .await
            .map(drop)
            .map_err(|error| database_error(&error));
    }
    sqlx::query(ROLE_SQL)
        .bind(ADMIN_ROLE)
        .execute(&mut *target)
        .await
        .map_err(|error| database_error(&error))?;
    sqlx::query(USER_ROLE_SQL)
        .bind(Uuid::from(*principal))
        .bind(ADMIN_ROLE)
        .execute(&mut *target)
        .await
        .map_err(|error| database_error(&error))?;
    Ok(())
}

/// The explicit environment of one authority projection.
#[derive(Debug, Clone, Copy)]
pub struct ApplicationTarget<'a> {
    pub org: &'a wamn_registry::OrgId,
    pub project: &'a wamn_registry::ProjectId,
    pub env: &'a wamn_registry::EnvName,
}

/// The authenticated actor of an authored application grant.
#[derive(Debug, Clone, Copy)]
pub enum GrantAuthority<'a> {
    User(&'a PrincipalId),
    Provisioning,
}

/// One authored role assignment or operation permission mutation.
#[derive(Debug, Clone, Copy)]
pub enum GrantChange<'a> {
    UserRole {
        user_id: &'a wamn_identity_protocol::PrincipalId,
        role: &'a str,
        grant: bool,
    },
    Permission {
        role: &'a str,
        reference: &'a str,
        grant: bool,
    },
}

/// The rows changed by an authorized application mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantOutcome {
    pub changed: bool,
    pub rows_added: u64,
}

/// Whether an authored permission can name this operation.
pub fn permission_is_grantable(reference: &str) -> bool {
    !wamn_routes::is_host_route_operation(reference)
        || wamn_routes::is_granted_host_route_operation(reference)
}

/// Mutate an authored grant inside the caller's application transaction.
/// The caller serializes role and permission writes with the application lock.
pub async fn mutate_grant(
    client: &mut sqlx::PgConnection,
    project_env_key: &wamn_registry::ProjectEnvKey,
    authority: GrantAuthority<'_>,
    change: GrantChange<'_>,
) -> Result<GrantOutcome, GrantError> {
    let result: Result<GrantOutcome, AdministrationError> = async {
    let (role, grant) = match change {
        GrantChange::UserRole { role, grant, .. } | GrantChange::Permission { role, grant, .. } => {
            (role, grant)
        }
    };
    if role == ADMIN_ROLE {
        return Err(match change {
            GrantChange::UserRole { user_id, .. } => AdministrationRefusal::AdminCovered {
                user_id: user_id.to_owned(),
            },
            GrantChange::Permission { .. } => AdministrationRefusal::AdminTakesNoPermission,
        }
        .into());
    }
    if !matches!(change, GrantChange::UserRole { grant: false, .. }) {
        require_role(client, role).await?;
    }
    if let GrantChange::Permission {
        reference,
        grant: true,
        ..
    } = change
        && !permission_is_grantable(reference)
    {
            return Err(AdministrationRefusal::NotGrantable {
                reference: reference.to_owned(),
            }
            .into());
    }
    if grant && let GrantAuthority::User(actor) = authority {
            let admin: bool = sqlx::query("SELECT EXISTS (SELECT 1 FROM platform.user_role WHERE user_id=$1 AND role_name='admin')").bind(Uuid::from(*actor)).fetch_one(&mut *client).await.map_err(|error| database_error(&error))?.get(0);
            if !admin {
                let required = match change {
                    GrantChange::Permission { reference, .. } => vec![reference.to_owned()],
                    GrantChange::UserRole { role, .. } => permission_rows(client, role).await?,
                };
                for reference in required {
                    let held: bool = sqlx::query("SELECT EXISTS (SELECT 1 FROM platform.user_role r JOIN platform.permission p ON p.role_name=r.role_name \
                         WHERE r.user_id=$1 AND p.permission=$2)").bind(Uuid::from(*actor)).bind(&reference).fetch_one(&mut *client).await.map_err(|error| database_error(&error))?.get(0);
                    if !held || !permission_is_grantable(&reference) {
                        return Err(AdministrationRefusal::NotGrantable { reference }.into());
                    }
                }
            }
    }
    let rows_added = match change {
        GrantChange::UserRole {
            user_id,
            role,
            grant,
        } => {
            if matches!(authority, GrantAuthority::User(_)) {
                require_user(client, user_id).await?;
            }
            let changed = if grant {
                grant_user_role(client, user_id, role).await?
            } else {
                revoke_user_role(client, user_id, role).await?
            };
            return Ok(GrantOutcome {
                changed,
                rows_added: u64::from(grant && changed),
            });
        }
        GrantChange::Permission {
            role,
            reference,
            grant: true,
        } => {
            grant_permission(client, role, reference)
                .await?
                .rows_added
        }
        GrantChange::Permission {
            role,
            reference,
            grant: false,
        } => {
            revoke_permission(client, role, reference).await?;
            return Ok(GrantOutcome {
                changed: true,
                rows_added: 0,
            });
        }
    };
    Ok(GrantOutcome {
        changed: rows_added != 0,
        rows_added,
    })
    }.await;
    result.map_err(|source| GrantError {
        project_env_key: project_env_key.to_owned(),
        source,
    })
}

/// A failed application mutation with its environment and original error.
#[derive(Debug)]
pub struct GrantError {
    project_env_key: wamn_registry::ProjectEnvKey,
    source: AdministrationError,
}

impl GrantError {
    pub fn refusal(&self) -> Option<&AdministrationRefusal> {
        match &self.source {
            AdministrationError::Refused(refusal) => Some(refusal),
            AdministrationError::Failed(_) => None,
        }
    }
}

impl fmt::Display for GrantError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "application grant in {}: {}",
            self.project_env_key, self.source
        )
    }
}

impl std::error::Error for GrantError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

impl From<GrantError> for AdministrationError {
    fn from(error: GrantError) -> Self {
        error.source
    }
}
