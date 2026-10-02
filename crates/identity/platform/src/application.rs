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

use std::collections::BTreeSet;
use std::fmt;

use tokio_postgres::GenericClient;

use wamn_project_state::ADMIN_ROLE;

use crate::{IdentityError, IdentityErrorType, PrincipalId, database_error};

/// One project database serves one tenant. Its platform rows, which
/// `reconcile-run-plane` writes first, name it.
const TENANT_SQL: &str =
    "SELECT DISTINCT tenant_id FROM app_system.users WHERE type = 'platform' ORDER BY 1";

const USER_ROW_SQL: &str = "INSERT INTO app_system.users \
    (tenant_id, id, type, email, display_name) VALUES ($1, $2::text::uuid, 'user', $3, $4) \
    ON CONFLICT (tenant_id, id) DO NOTHING";

const ROLE_SQL: &str = "INSERT INTO app_system.roles (tenant_id, name) VALUES ($1, $2) \
    ON CONFLICT (tenant_id, name) DO NOTHING";

const USER_ROLE_SQL: &str = "INSERT INTO app_system.user_roles (tenant_id, user_id, role_name) \
    VALUES ($1, $2::text::uuid, $3) ON CONFLICT DO NOTHING";

const DROP_USER_ROLE_SQL: &str = "DELETE FROM app_system.user_roles \
    WHERE tenant_id = $1 AND user_id = $2::text::uuid AND role_name = $3";

const DROP_USER_ROW_SQL: &str = "DELETE FROM app_system.users \
    WHERE tenant_id = $1 AND id = $2::text::uuid AND type = 'user'";

/// The user an application row names.
#[derive(Debug, Clone, Copy)]
pub struct ApplicationUser<'a> {
    pub principal_id: &'a PrincipalId,
    pub email: &'a str,
    pub display_name: &'a str,
}

/// The tenant of the project database. A database without platform rows, or
/// with more than one tenant, is refused.
pub async fn environment_tenant(
    client: &(impl GenericClient + Sync),
) -> Result<String, IdentityError> {
    let tenants: Vec<String> = client
        .query(TENANT_SQL, &[])
        .await
        .map_err(|error| database_error(&error))?
        .iter()
        .map(|row| row.get(0))
        .collect();
    match <[String; 1]>::try_from(tenants) {
        Ok([tenant]) => Ok(tenant),
        Err(tenants) if tenants.is_empty() => Err(IdentityError::new(
            IdentityErrorType::NotFound,
            "the project database has no platform rows. Run reconcile-run-plane first",
        )),
        Err(tenants) => Err(IdentityError::new(
            IdentityErrorType::Conflict,
            format!(
                "the project database names {} tenants in its platform rows",
                tenants.len()
            ),
        )),
    }
}

/// Write the user row of `user`, and keep a row that exists.
pub async fn write_user(
    client: &(impl GenericClient + Sync),
    tenant: &str,
    user: ApplicationUser<'_>,
) -> Result<(), IdentityError> {
    client
        .execute(
            USER_ROW_SQL,
            &[
                &tenant,
                &user.principal_id.as_str(),
                &user.email,
                &user.display_name,
            ],
        )
        .await
        .map_err(|error| database_error(&error))?;
    Ok(())
}

/// Write the user row and the `admin` row of `user`, and the `admin` role
/// when the tenant has none yet.
pub async fn write_admin(
    client: &(impl GenericClient + Sync),
    tenant: &str,
    user: ApplicationUser<'_>,
) -> Result<(), IdentityError> {
    write_user(client, tenant, user).await?;
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
) -> Result<(), IdentityError> {
    client
        .execute(
            DROP_USER_ROLE_SQL,
            &[&tenant, &principal_id.as_str(), &ADMIN_ROLE],
        )
        .await
        .map_err(|error| database_error(&error))?;
    Ok(())
}

/// Remove the user row of the principal. Its foreign keys remove its role
/// rows and its API keys.
pub async fn remove_user(
    client: &(impl GenericClient + Sync),
    tenant: &str,
    principal_id: &PrincipalId,
) -> Result<(), IdentityError> {
    client
        .execute(DROP_USER_ROW_SQL, &[&tenant, &principal_id.as_str()])
        .await
        .map_err(|error| database_error(&error))?;
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
    /// The current serving release does not serve the operation.
    OperationNotServed { reference: String },
    /// The role holds the operation neither directly nor through a root.
    NotHeld { role: String, reference: String },
    /// The role holds the operation only because these roots require it.
    NotSelected {
        role: String,
        reference: String,
        required_by: Vec<String>,
    },
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
            Self::OperationNotServed { reference } => write!(
                formatter,
                "the current serving release does not serve the operation {reference}"
            ),
            Self::NotHeld { role, reference } => {
                write!(formatter, "role {role} does not hold {reference}")
            }
            Self::NotSelected {
                role,
                reference,
                required_by,
            } => write!(
                formatter,
                "{reference} is not directly granted to role {role}; it is required by {}",
                required_by.join(", ")
            ),
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
    /// Rows written: the selected root and each required permission not yet
    /// held through it. A second grant writes none.
    pub rows_added: u64,
    /// The closure of the root in the current serving release, the root
    /// included.
    pub closure: BTreeSet<String>,
}

/// What a permission revoke changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRevokeOutcome {
    /// The other selected roots of the role that still require the revoked
    /// permission, so it stays effective through them.
    pub still_required_by: Vec<String>,
}

const ROLE_EXISTS_SQL: &str = "SELECT 1 FROM app_system.roles WHERE tenant_id = $1 AND name = $2";

const DROP_ROLE_SQL: &str = "DELETE FROM app_system.roles WHERE tenant_id = $1 AND name = $2";

const ROOT_SQL: &str = "INSERT INTO app_system.permissions \
    (tenant_id, role_name, permission, required_by) VALUES ($1, $2, $3, $3) \
    ON CONFLICT DO NOTHING";

const REQUIRED_SQL: &str = "INSERT INTO app_system.permissions \
    (tenant_id, role_name, permission, required_by) \
    SELECT $1, $2, permission, $3 FROM unnest($4::text[]) AS permission \
     WHERE permission <> $3 ON CONFLICT DO NOTHING";

const DROP_ROOT_SQL: &str = "DELETE FROM app_system.permissions \
    WHERE tenant_id = $1 AND role_name = $2 AND permission = $3 AND required_by = $3";

const REQUIRED_BY_SQL: &str = "SELECT required_by FROM app_system.permissions \
    WHERE tenant_id = $1 AND role_name = $2 AND permission = $3 ORDER BY required_by";

// The writes below run in a transaction that the caller prepared: it holds
// the tenant's operation grant lock and binds the actor of the writes.
// `wamn-ctl` binds `wamn:provisioning`, and a host route binds its caller.

/// Create the empty authored role `role`. A second create changes nothing.
pub async fn create_role(
    client: &(impl GenericClient + Sync),
    tenant: &str,
    role: &str,
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
    Ok(written == 1)
}

/// Delete the authored role `role` with its assignments and permissions. A
/// delete of an absent role changes nothing.
pub async fn delete_role(
    client: &(impl GenericClient + Sync),
    tenant: &str,
    role: &str,
) -> Result<bool, AdministrationError> {
    if role == ADMIN_ROLE {
        return Err(AdministrationRefusal::AdminDeleted.into());
    }
    let removed = client
        .execute(DROP_ROLE_SQL, &[&tenant, &role])
        .await
        .map_err(|error| database_error(&error))?;
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

/// Select `reference` for the authored `role` and write `closure`, its
/// closure in the current serving release, root included. `None` means that
/// the release does not serve the operation.
pub async fn grant_permission(
    client: &(impl GenericClient + Sync),
    tenant: &str,
    role: &str,
    reference: &str,
    closure: Option<&BTreeSet<String>>,
) -> Result<PermissionGrantOutcome, AdministrationError> {
    if role == ADMIN_ROLE {
        return Err(AdministrationRefusal::AdminTakesNoPermission.into());
    }
    require_role(client, tenant, role).await?;
    let closure = closure.ok_or_else(|| AdministrationRefusal::OperationNotServed {
        reference: reference.to_owned(),
    })?;
    let permissions: Vec<&str> = closure.iter().map(String::as_str).collect();
    // The root row goes first, because every closure row references it.
    let mut rows_added = client
        .execute(ROOT_SQL, &[&tenant, &role, &reference])
        .await
        .map_err(|error| database_error(&error))?;
    rows_added += client
        .execute(REQUIRED_SQL, &[&tenant, &role, &reference, &permissions])
        .await
        .map_err(|error| database_error(&error))?;
    Ok(PermissionGrantOutcome {
        rows_added,
        closure: closure.clone(),
    })
}

/// Remove the selection of `reference` from `role` and every permission that
/// the selection required. A permission another selected root still requires
/// stays, and the outcome names those roots. A reference that the role holds
/// only because another root requires it refuses and names those roots.
pub async fn revoke_permission(
    client: &(impl GenericClient + Sync),
    tenant: &str,
    role: &str,
    reference: &str,
) -> Result<PermissionRevokeOutcome, AdministrationError> {
    require_role(client, tenant, role).await?;
    let removed = client
        .execute(DROP_ROOT_SQL, &[&tenant, &role, &reference])
        .await
        .map_err(|error| database_error(&error))?;
    let still_required_by: Vec<String> = client
        .query(REQUIRED_BY_SQL, &[&tenant, &role, &reference])
        .await
        .map_err(|error| database_error(&error))?
        .iter()
        .map(|row| row.get(0))
        .collect();
    if removed == 0 {
        return Err(if still_required_by.is_empty() {
            AdministrationRefusal::NotHeld {
                role: role.to_owned(),
                reference: reference.to_owned(),
            }
        } else {
            AdministrationRefusal::NotSelected {
                role: role.to_owned(),
                reference: reference.to_owned(),
                required_by: still_required_by,
            }
        }
        .into());
    }
    Ok(PermissionRevokeOutcome { still_required_by })
}

/// Give the existing role `role` to the user `user_id`. A second grant
/// changes nothing.
pub async fn grant_user_role(
    client: &(impl GenericClient + Sync),
    tenant: &str,
    user_id: &str,
    role: &str,
) -> Result<bool, AdministrationError> {
    require_role(client, tenant, role).await?;
    let written = client
        .execute(USER_ROLE_SQL, &[&tenant, &user_id, &role])
        .await
        .map_err(|error| database_error(&error))?;
    Ok(written == 1)
}

/// Take the role `role` from the user `user_id`. A revoke of a role the user
/// does not hold changes nothing.
pub async fn revoke_user_role(
    client: &(impl GenericClient + Sync),
    tenant: &str,
    user_id: &str,
    role: &str,
) -> Result<bool, AdministrationError> {
    let removed = client
        .execute(DROP_USER_ROLE_SQL, &[&tenant, &user_id, &role])
        .await
        .map_err(|error| database_error(&error))?;
    Ok(removed == 1)
}
