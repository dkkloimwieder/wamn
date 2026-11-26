//! Application roles, grants and administration.

mod sql;

pub use sql::{
    ProjectAuthority, app_users, project_administration, read_project_authority,
    require_platform_rows, role_names, write_env_status,
};

use std::fmt;

use wamn_identity_protocol::{ADMIN_ROLE, PrincipalId};

use crate::identity_error::IdentityError;

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

/// One application user with the roles it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppUserRow {
    pub id: PrincipalId,
    pub email: String,
    pub display_name: Option<String>,
    pub roles: Vec<String>,
}

/// The explicit environment of one authority projection.
#[derive(Debug, Clone, Copy)]
pub struct AppTarget<'a> {
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
    Ok(sql::insert_role(client, role).await?)
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
    Ok(sql::remove_role(client, role).await?)
}

/// Refuse unless `role` exists in the environment.
async fn require_role(
    client: &mut sqlx::PgConnection,
    role: &str,
) -> Result<(), AdministrationError> {
    let exists = sql::role_exists(client, role).await?;
    if exists {
        Ok(())
    } else {
        Err(AdministrationRefusal::RoleNotFound {
            role: role.to_owned(),
        }
        .into())
    }
}

/// Lock the role recipient against concurrent deactivation.
async fn require_user(
    client: &mut sqlx::PgConnection,
    user_id: &wamn_identity_protocol::PrincipalId,
) -> Result<(), AdministrationError> {
    let exists = sql::lock_grant_recipient(client, user_id).await?;
    if exists {
        Ok(())
    } else {
        Err(AdministrationRefusal::UserNotFound { user_id: *user_id }.into())
    }
}

/// The stored permissions held by an existing role.
pub async fn permission_rows(
    client: &mut sqlx::PgConnection,
    role: &str,
) -> Result<Vec<String>, AdministrationError> {
    require_role(client, role).await?;
    Ok(sql::permissions(client, role).await?)
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
            GrantChange::UserRole { role, grant, .. }
            | GrantChange::Permission { role, grant, .. } => (role, grant),
        };
        if role == ADMIN_ROLE {
            return Err(match change {
                GrantChange::UserRole { user_id, .. } => {
                    AdministrationRefusal::AdminCovered { user_id: *user_id }
                }
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
            let admin = sql::is_admin(client, actor).await?;
            if !admin {
                let required = match change {
                    GrantChange::Permission { reference, .. } => vec![reference.to_owned()],
                    GrantChange::UserRole { role, .. } => permission_rows(client, role).await?,
                };
                for reference in required {
                    let held = sql::holds_permission(client, actor, &reference).await?;
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
                    sql::assign_role(client, user_id, role).await?
                } else {
                    sql::unassign_role(client, user_id, role).await?
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
                sql::insert_permission(client, role, reference)
                    .await?
                    .rows_added
            }
            GrantChange::Permission {
                role,
                reference,
                grant: false,
            } => {
                if !sql::remove_permission(client, role, reference).await? {
                    return Err(AdministrationRefusal::NotHeld {
                        role: role.to_owned(),
                        reference: reference.to_owned(),
                    }
                    .into());
                }
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
    }
    .await;
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

/// The user an application row names.
#[derive(Debug, Clone, Copy)]
pub struct AppUser<'a> {
    pub principal_id: &'a PrincipalId,
    pub email: &'a str,
    pub display_name: &'a str,
}
