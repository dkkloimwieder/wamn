//! The application rows of org and project administration in one project
//! database (docs/plan/platform-ui.md §4.4, application writes).
//!
//! The control host calls these with the environment's `wamn_administration`
//! login, in one transaction per environment that binds the caller and the
//! route's sealed operation id. A grant writes them after its system
//! transaction, and a revoke or a deactivation removes them before its system
//! transaction commits.
//!
//! It also holds the role, permission and user role writes of §4.6, which
//! `wamn-ctl` and the application host routes share.

use std::fmt;

use tokio_postgres::GenericClient;

use wamn_project_state::ADMIN_ROLE;

use crate::auth_changes::AuthDrops;
use crate::{IdentityError, IdentityErrorType, PrincipalId, database_error};

/// Whether the project database has its platform rows, which `env apply`
/// writes first.
const PLATFORM_ROWS_SQL: &str =
    "SELECT EXISTS (SELECT FROM platform.\"user\" WHERE type = 'platform')";

const USER_ROW_SQL: &str = "INSERT INTO platform.\"user\" \
    (id, type, email, display_name) VALUES ($1::text::uuid, 'user', $2, $3) \
    ON CONFLICT (id) DO NOTHING";

const SERVICE_ROW_SQL: &str = "INSERT INTO platform.\"user\" \
    (id, type, email, display_name) VALUES ($1::text::uuid, 'service', $2, $3)";

const ROLE_SQL: &str = "INSERT INTO platform.role (tenant_id, name) VALUES ($1, $2) \
    ON CONFLICT (tenant_id, name) DO NOTHING";

const USER_ROLE_SQL: &str = "INSERT INTO platform.user_role (tenant_id, user_id, role_name) \
    VALUES ($1, $2::text::uuid, $3) ON CONFLICT DO NOTHING";

const DROP_USER_ROLE_SQL: &str = "DELETE FROM platform.user_role \
    WHERE tenant_id = $1 AND user_id = $2::text::uuid AND role_name = $3";

const DROP_USER_ROW_SQL: &str = "DELETE FROM platform.\"user\" \
    WHERE id = $1::text::uuid AND type = 'user'";

/// The status row of the environment. No row means active.
const ENVIRONMENT_STATUS_ROW_SQL: &str = "INSERT INTO platform.environment \
    (tenant_id, status) VALUES ($1, $2) \
    ON CONFLICT (tenant_id) DO UPDATE SET status = EXCLUDED.status \
    WHERE platform.environment.status <> EXCLUDED.status";

/// The user an application row names.
#[derive(Debug, Clone, Copy)]
pub struct ApplicationUser<'a> {
    pub principal_id: &'a PrincipalId,
    pub email: &'a str,
    pub display_name: &'a str,
}

/// Refuse a project database without platform rows. The database is the org
/// boundary, so its rows name no tenant: the caller derives the tenant of
/// the environment from its triple (`project_env_tenant`).
pub async fn require_platform_rows(
    client: &(impl GenericClient + Sync),
) -> Result<(), IdentityError> {
    let present: bool = client
        .query_one(PLATFORM_ROWS_SQL, &[])
        .await
        .map_err(|error| database_error(&error))?
        .get(0);
    if present {
        Ok(())
    } else {
        Err(IdentityError::new(
            IdentityErrorType::NotFound,
            "the project database has no platform rows. Run `wamn-ctl env apply` first",
        ))
    }
}

/// Write the status row of the environment, the mirror of its system row
/// that its host reads (docs/plan/platform-ui.md §5.4).
pub async fn write_environment_status(
    client: &(impl GenericClient + Sync),
    tenant: &str,
    status: crate::org::EnvironmentStatus,
    drops: &mut AuthDrops,
) -> Result<(), IdentityError> {
    client
        .execute(ENVIRONMENT_STATUS_ROW_SQL, &[&tenant, &status.as_str()])
        .await
        .map_err(|error| database_error(&error))?;
    drops.tenant(tenant);
    Ok(())
}

/// Write the user row of `user`, and keep a row that exists.
pub async fn write_user(
    client: &(impl GenericClient + Sync),
    tenant: &str,
    user: ApplicationUser<'_>,
    drops: &mut AuthDrops,
) -> Result<(), IdentityError> {
    client
        .execute(
            USER_ROW_SQL,
            &[&user.principal_id.as_str(), &user.email, &user.display_name],
        )
        .await
        .map_err(|error| database_error(&error))?;
    drops.principal(tenant, user.principal_id.as_str());
    Ok(())
}

/// Write the user row of the service principal `service`.
pub async fn write_service(
    client: &(impl GenericClient + Sync),
    tenant: &str,
    service: ApplicationUser<'_>,
    drops: &mut AuthDrops,
) -> Result<(), IdentityError> {
    client
        .execute(
            SERVICE_ROW_SQL,
            &[
                &service.principal_id.as_str(),
                &service.email,
                &service.display_name,
            ],
        )
        .await
        .map_err(|error| database_error(&error))?;
    drops.principal(tenant, service.principal_id.as_str());
    Ok(())
}

/// Write the user row and the `admin` row of `user`, and the `admin` role
/// when the tenant has none yet.
pub async fn write_admin(
    client: &(impl GenericClient + Sync),
    tenant: &str,
    user: ApplicationUser<'_>,
    drops: &mut AuthDrops,
) -> Result<(), IdentityError> {
    write_user(client, tenant, user, drops).await?;
    client
        .execute(ROLE_SQL, &[&tenant, &ADMIN_ROLE])
        .await
        .map_err(|error| database_error(&error))?;
    client
        .execute(
            USER_ROLE_SQL,
            &[&tenant, &user.principal_id.as_str(), &ADMIN_ROLE],
        )
        .await
        .map_err(|error| database_error(&error))?;
    Ok(())
}

/// Remove the `admin` row of the principal. Its other roles stay.
pub async fn remove_admin(
    client: &(impl GenericClient + Sync),
    tenant: &str,
    principal_id: &PrincipalId,
    drops: &mut AuthDrops,
) -> Result<(), IdentityError> {
    client
        .execute(
            DROP_USER_ROLE_SQL,
            &[&tenant, &principal_id.as_str(), &ADMIN_ROLE],
        )
        .await
        .map_err(|error| database_error(&error))?;
    drops.principal(tenant, principal_id.as_str());
    Ok(())
}

/// Remove the user row of the principal. Its foreign keys remove its role
/// rows and its API keys.
pub async fn remove_user(
    client: &(impl GenericClient + Sync),
    tenant: &str,
    principal_id: &PrincipalId,
    drops: &mut AuthDrops,
) -> Result<(), IdentityError> {
    client
        .execute(DROP_USER_ROW_SQL, &[&principal_id.as_str()])
        .await
        .map_err(|error| database_error(&error))?;
    drops.principal(tenant, principal_id.as_str());
    Ok(())
}

/// Why an application administration write refused (docs/plan/platform-ui.md
/// §2.4 and §4.6). Each refusal changes no row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdministrationRefusal {
    /// `admin` holds every operation the release serves.
    AdminTakesNoPermission,
    /// `apply-package` creates `admin`, so no caller creates it.
    AdminCreated,
    /// `admin` is the built-in role.
    AdminDeleted,
    /// The name is not a role slug.
    NotRoleName { role: String },
    /// The role does not exist in the tenant.
    RoleNotFound { role: String, tenant: String },
    /// The operation is fixed to `admin` or to every member (§2.5).
    NotGrantable { reference: String },
    /// The tenant has no application user with the id.
    UserNotFound { user_id: String },
    /// `admin` stays while the user holds `project-admin` in the project,
    /// which `org-admin` also writes (§2.8).
    AdminCovered { user_id: String },
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
            Self::NotRoleName { role } => write!(
                formatter,
                "role {role:?} is not a role name: lowercase letters, digits and hyphens after \
                 the first character, at most 64 bytes"
            ),
            Self::RoleNotFound { role, tenant } => {
                write!(formatter, "role {role} does not exist in tenant {tenant}")
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

const ROLE_EXISTS_SQL: &str = "SELECT 1 FROM platform.role WHERE tenant_id = $1 AND name = $2";

const DROP_ROLE_SQL: &str = "DELETE FROM platform.role WHERE tenant_id = $1 AND name = $2";

const PERMISSION_SQL: &str = "INSERT INTO platform.permission \
    (tenant_id, role_name, permission) VALUES ($1, $2, $3) \
    ON CONFLICT DO NOTHING";

const DROP_PERMISSION_SQL: &str = "DELETE FROM platform.permission \
    WHERE tenant_id = $1 AND role_name = $2 AND permission = $3";

// The writes below run in a transaction that the caller prepared: it holds
// the tenant's operation grant lock and binds the actor of the writes.
// `wamn-ctl` binds `wamn:provisioning`, and a host route binds its caller.

/// Create the empty authored role `role`. A second create changes nothing.
pub async fn create_role(
    client: &(impl GenericClient + Sync),
    tenant: &str,
    role: &str,
    drops: &mut AuthDrops,
) -> Result<bool, AdministrationError> {
    if role == ADMIN_ROLE {
        return Err(AdministrationRefusal::AdminCreated.into());
    }
    if !wamn_session::token::is_role_slug(role) {
        return Err(AdministrationRefusal::NotRoleName {
            role: role.to_owned(),
        }
        .into());
    }
    let written = client
        .execute(ROLE_SQL, &[&tenant, &role])
        .await
        .map_err(|error| database_error(&error))?;
    drops.tenant(tenant);
    Ok(written == 1)
}

/// Delete the authored role `role` with its assignments and permissions. A
/// delete of an absent role changes nothing.
pub async fn delete_role(
    client: &(impl GenericClient + Sync),
    tenant: &str,
    role: &str,
    drops: &mut AuthDrops,
) -> Result<bool, AdministrationError> {
    if role == ADMIN_ROLE {
        return Err(AdministrationRefusal::AdminDeleted.into());
    }
    let removed = client
        .execute(DROP_ROLE_SQL, &[&tenant, &role])
        .await
        .map_err(|error| database_error(&error))?;
    drops.tenant(tenant);
    Ok(removed == 1)
}

/// Refuse unless `role` exists in the tenant.
pub async fn require_role(
    client: &(impl GenericClient + Sync),
    tenant: &str,
    role: &str,
) -> Result<(), AdministrationError> {
    let exists = client
        .query_opt(ROLE_EXISTS_SQL, &[&tenant, &role])
        .await
        .map_err(|error| database_error(&error))?
        .is_some();
    if exists {
        Ok(())
    } else {
        Err(AdministrationRefusal::RoleNotFound {
            role: role.to_owned(),
            tenant: tenant.to_owned(),
        }
        .into())
    }
}

/// Grant `reference` to the authored `role`: store it as a row. The grant
/// stores any reference that `permission_reference_check` accepts, whether a
/// release serves it or not.
pub async fn grant_permission(
    client: &(impl GenericClient + Sync),
    tenant: &str,
    role: &str,
    reference: &str,
    drops: &mut AuthDrops,
) -> Result<PermissionGrantOutcome, AdministrationError> {
    if role == ADMIN_ROLE {
        return Err(AdministrationRefusal::AdminTakesNoPermission.into());
    }
    require_role(client, tenant, role).await?;
    let rows_added = client
        .execute(PERMISSION_SQL, &[&tenant, &role, &reference])
        .await
        .map_err(|error| database_error(&error))?;
    drops.tenant(tenant);
    Ok(PermissionGrantOutcome { rows_added })
}

/// Remove the row `reference` from `role`. A revoke of a reference the role
/// does not hold refuses.
pub async fn revoke_permission(
    client: &(impl GenericClient + Sync),
    tenant: &str,
    role: &str,
    reference: &str,
    drops: &mut AuthDrops,
) -> Result<(), AdministrationError> {
    require_role(client, tenant, role).await?;
    let removed = client
        .execute(DROP_PERMISSION_SQL, &[&tenant, &role, &reference])
        .await
        .map_err(|error| database_error(&error))?;
    if removed == 0 {
        return Err(AdministrationRefusal::NotHeld {
            role: role.to_owned(),
            reference: reference.to_owned(),
        }
        .into());
    }
    drops.tenant(tenant);
    Ok(())
}

/// Give the existing role `role` to the user `user_id`. A second grant
/// changes nothing.
pub async fn grant_user_role(
    client: &(impl GenericClient + Sync),
    tenant: &str,
    user_id: &str,
    role: &str,
    drops: &mut AuthDrops,
) -> Result<bool, AdministrationError> {
    require_role(client, tenant, role).await?;
    let written = client
        .execute(USER_ROLE_SQL, &[&tenant, &user_id, &role])
        .await
        .map_err(|error| database_error(&error))?;
    drops.principal(tenant, user_id);
    Ok(written == 1)
}

/// Take the role `role` from the user `user_id`. A revoke of a role the user
/// does not hold changes nothing.
pub async fn revoke_user_role(
    client: &(impl GenericClient + Sync),
    tenant: &str,
    user_id: &str,
    role: &str,
    drops: &mut AuthDrops,
) -> Result<bool, AdministrationError> {
    let removed = client
        .execute(DROP_USER_ROLE_SQL, &[&tenant, &user_id, &role])
        .await
        .map_err(|error| database_error(&error))?;
    drops.principal(tenant, user_id);
    Ok(removed == 1)
}

const USER_EXISTS_SQL: &str = "SELECT 1 FROM platform.\"user\" \
    WHERE id = $1::text::uuid AND type = 'user'";

const USER_LIST_SQL: &str = "SELECT u.id::text, u.email, u.display_name, \
    COALESCE(array_agg(r.role_name ORDER BY r.role_name) \
             FILTER (WHERE r.role_name IS NOT NULL), '{}') \
    FROM platform.\"user\" u \
    LEFT JOIN platform.user_role r ON r.tenant_id = $1 AND r.user_id = u.id \
    WHERE u.type = 'user' \
    GROUP BY u.id, u.email, u.display_name ORDER BY u.email, u.id";

const ROLE_LIST_SQL: &str = "SELECT name FROM platform.role WHERE tenant_id = $1 ORDER BY name";

const PERMISSION_LIST_SQL: &str = "SELECT permission FROM platform.permission \
    WHERE tenant_id = $1 AND role_name = $2 ORDER BY permission";

/// One application user with the roles it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationUserRow {
    pub id: String,
    pub email: String,
    pub display_name: Option<String>,
    pub roles: Vec<String>,
}

/// Refuse unless the database has the application user `user_id`.
pub async fn require_user(
    client: &(impl GenericClient + Sync),
    user_id: &str,
) -> Result<(), AdministrationError> {
    let exists = client
        .query_opt(USER_EXISTS_SQL, &[&user_id])
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

/// The application users of the tenant with their roles, by email.
pub async fn application_users(
    client: &(impl GenericClient + Sync),
    tenant: &str,
) -> Result<Vec<ApplicationUserRow>, AdministrationError> {
    Ok(client
        .query(USER_LIST_SQL, &[&tenant])
        .await
        .map_err(|error| database_error(&error))?
        .iter()
        .map(|row| ApplicationUserRow {
            id: row.get(0),
            email: row.get(1),
            display_name: row.get(2),
            roles: row.get(3),
        })
        .collect())
}

/// The role names of the tenant, `admin` included.
pub async fn role_names(
    client: &(impl GenericClient + Sync),
    tenant: &str,
) -> Result<Vec<String>, AdministrationError> {
    Ok(client
        .query(ROLE_LIST_SQL, &[&tenant])
        .await
        .map_err(|error| database_error(&error))?
        .iter()
        .map(|row| row.get(0))
        .collect())
}

/// The stored permissions of `role`.
pub async fn permission_rows(
    client: &(impl GenericClient + Sync),
    tenant: &str,
    role: &str,
) -> Result<Vec<String>, AdministrationError> {
    require_role(client, tenant, role).await?;
    Ok(client
        .query(PERMISSION_LIST_SQL, &[&tenant, &role])
        .await
        .map_err(|error| database_error(&error))?
        .iter()
        .map(|row| row.get(0))
        .collect())
}
