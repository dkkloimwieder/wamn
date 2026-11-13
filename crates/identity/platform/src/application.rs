//! Project database identities and authored application grants
//! (docs/architecture/overview.md#organization-membership-and-administration).
//!
//! System writers commit authority before projection. Projection reads current
//! authority under the org membership lock and commits application data before
//! releasing that lock. Authored grants use `mutate_grant` inside the application
//! transaction. The CLI calls the HTTP routes.

use std::fmt;

use tokio_postgres::GenericClient;

use wamn_identity_protocol::ADMIN_ROLE;

use crate::{IdentityError, IdentityErrorType, PrincipalId, database_error};
use wamn_broker::auth_changes::AuthDrops;

/// Whether the project database has its platform rows, which `env apply`
/// writes first.
const PLATFORM_ROWS_SQL: &str =
    "SELECT EXISTS (SELECT FROM platform.\"user\" WHERE type = 'platform')";

const USER_ROW_SQL: &str = "INSERT INTO platform.\"user\" \
    (id, type, email, display_name) VALUES ($1::text::uuid, 'user', $2, $3) \
    ON CONFLICT (id) DO NOTHING";

const SERVICE_ROW_SQL: &str = "INSERT INTO platform.\"user\" \
    (id, type, email, display_name) VALUES ($1::text::uuid, 'service', $2, $3)";

const ROLE_SQL: &str = "INSERT INTO platform.role (name) VALUES ($1) \
    ON CONFLICT (name) DO NOTHING";

const USER_ROLE_SQL: &str = "INSERT INTO platform.user_role (user_id, role_name) \
    VALUES ($1::text::uuid, $2) ON CONFLICT DO NOTHING";

const DROP_USER_ROLE_SQL: &str = "DELETE FROM platform.user_role \
    WHERE user_id = $1::text::uuid AND role_name = $2";

const DROP_USER_ROW_SQL: &str = "DELETE FROM platform.\"user\" \
    WHERE id = $1::text::uuid AND type = 'user'";

/// The status row of the environment. No row means active.
const ENVIRONMENT_STATUS_ROW_SQL: &str = "INSERT INTO platform.environment \
    (status) VALUES ($1) \
    ON CONFLICT (id) DO UPDATE SET status = EXCLUDED.status \
    WHERE platform.environment.status <> EXCLUDED.status";

/// The user an application row names.
#[derive(Debug, Clone, Copy)]
pub struct ApplicationUser<'a> {
    pub principal_id: &'a PrincipalId,
    pub email: &'a str,
    pub display_name: &'a str,
}

/// Refuse a project database without platform rows. The database is the org
/// boundary, so its rows name no project: the caller derives the project of
/// the environment from its triple (`project_env_key`).
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

/// Write the status row of the environment, the mirror of its system row that
/// its host reads (docs/architecture/execution.md#routing-and-availability).
pub async fn write_environment_status(
    client: &(impl GenericClient + Sync),
    project_env_key: &str,
    status: crate::org::EnvironmentStatus,
    drops: &mut AuthDrops,
) -> Result<(), IdentityError> {
    client
        .execute(ENVIRONMENT_STATUS_ROW_SQL, &[&status.as_str()])
        .await
        .map_err(|error| database_error(&error))?;
    drops.project_env_key(project_env_key);
    Ok(())
}

/// Write the user row of `user`, and keep a row that exists.
pub async fn write_user(
    client: &(impl GenericClient + Sync),
    project_env_key: &str,
    user: ApplicationUser<'_>,
    drops: &mut AuthDrops,
) -> Result<(), IdentityError> {
    client
        .execute(
            USER_ROW_SQL,
            &[
                &user.principal_id.to_string().as_str(),
                &user.email,
                &user.display_name,
            ],
        )
        .await
        .map_err(|error| database_error(&error))?;
    drops.principal(project_env_key, user.principal_id.to_string().as_str());
    Ok(())
}

/// Write the user row of the service principal `service`.
pub async fn write_service(
    client: &(impl GenericClient + Sync),
    project_env_key: &str,
    service: ApplicationUser<'_>,
    drops: &mut AuthDrops,
) -> Result<(), IdentityError> {
    client
        .execute(
            SERVICE_ROW_SQL,
            &[
                &service.principal_id.to_string().as_str(),
                &service.email,
                &service.display_name,
            ],
        )
        .await
        .map_err(|error| database_error(&error))?;
    drops.principal(project_env_key, service.principal_id.to_string().as_str());
    Ok(())
}

/// Remove the `admin` row of the principal. Its other roles stay.
pub async fn remove_admin(
    client: &(impl GenericClient + Sync),
    project_env_key: &str,
    principal_id: &PrincipalId,
    drops: &mut AuthDrops,
) -> Result<(), IdentityError> {
    client
        .execute(
            DROP_USER_ROLE_SQL,
            &[&principal_id.to_string().as_str(), &ADMIN_ROLE],
        )
        .await
        .map_err(|error| database_error(&error))?;
    drops.principal(project_env_key, principal_id.to_string().as_str());
    Ok(())
}

/// Remove the user row of the principal. Its foreign keys remove its role
/// rows and its API keys.
pub async fn remove_user(
    client: &(impl GenericClient + Sync),
    project_env_key: &str,
    principal_id: &PrincipalId,
    drops: &mut AuthDrops,
) -> Result<(), IdentityError> {
    client
        .execute(DROP_USER_ROW_SQL, &[&principal_id.to_string().as_str()])
        .await
        .map_err(|error| database_error(&error))?;
    drops.principal(project_env_key, principal_id.to_string().as_str());
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
    UserNotFound { user_id: String },
    /// `admin` stays while the user holds `project-admin` in the project,
    /// including administration inherited from its org or environment.
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
// `wamn-ctl` binds `wamn:provisioning`, and a host route binds its caller.

/// Create the empty authored role `role`. A second create changes nothing.
pub async fn create_role(
    client: &(impl GenericClient + Sync),
    project_env_key: &str,
    role: &str,
    drops: &mut AuthDrops,
) -> Result<bool, AdministrationError> {
    if role == ADMIN_ROLE {
        return Err(AdministrationRefusal::AdminCreated.into());
    }
    if !role
        .parse::<wamn_identity_protocol::RoleName>()
        .is_ok_and(|parsed| parsed.as_str() == role)
    {
        return Err(AdministrationRefusal::NotProjectRole {
            role: role.to_owned(),
        }
        .into());
    }
    let written = client
        .execute(ROLE_SQL, &[&role])
        .await
        .map_err(|error| database_error(&error))?;
    drops.project_env_key(project_env_key);
    Ok(written == 1)
}

/// Delete the authored role `role` with its assignments and permissions. A
/// delete of an absent role changes nothing.
pub async fn delete_role(
    client: &(impl GenericClient + Sync),
    project_env_key: &str,
    role: &str,
    drops: &mut AuthDrops,
) -> Result<bool, AdministrationError> {
    if role == ADMIN_ROLE {
        return Err(AdministrationRefusal::AdminDeleted.into());
    }
    let removed = client
        .execute(DROP_ROLE_SQL, &[&role])
        .await
        .map_err(|error| database_error(&error))?;
    drops.project_env_key(project_env_key);
    Ok(removed == 1)
}

/// Refuse unless `role` exists in the environment.
pub async fn require_role(
    client: &(impl GenericClient + Sync),
    role: &str,
) -> Result<(), AdministrationError> {
    let exists = client
        .query_opt(ROLE_EXISTS_SQL, &[&role])
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
    client: &(impl GenericClient + Sync),
    project_env_key: &str,
    role: &str,
    reference: &str,
    drops: &mut AuthDrops,
) -> Result<PermissionGrantOutcome, AdministrationError> {
    let rows_added = client
        .execute(PERMISSION_SQL, &[&role, &reference])
        .await
        .map_err(|error| database_error(&error))?;
    drops.project_env_key(project_env_key);
    Ok(PermissionGrantOutcome { rows_added })
}

/// Remove the row `reference` from `role`. A revoke of a reference the role
/// does not hold refuses.
async fn revoke_permission(
    client: &(impl GenericClient + Sync),
    project_env_key: &str,
    role: &str,
    reference: &str,
    drops: &mut AuthDrops,
) -> Result<(), AdministrationError> {
    let removed = client
        .execute(DROP_PERMISSION_SQL, &[&role, &reference])
        .await
        .map_err(|error| database_error(&error))?;
    if removed == 0 {
        return Err(AdministrationRefusal::NotHeld {
            role: role.to_owned(),
            reference: reference.to_owned(),
        }
        .into());
    }
    drops.project_env_key(project_env_key);
    Ok(())
}

/// Give the existing role `role` to the user `user_id`. A second grant
/// changes nothing.
async fn grant_user_role(
    client: &(impl GenericClient + Sync),
    project_env_key: &str,
    user_id: &str,
    role: &str,
    drops: &mut AuthDrops,
) -> Result<bool, AdministrationError> {
    let written = client
        .execute(USER_ROLE_SQL, &[&user_id, &role])
        .await
        .map_err(|error| database_error(&error))?;
    drops.principal(project_env_key, user_id);
    Ok(written == 1)
}

/// Take the role `role` from the user `user_id`. A revoke of a role the user
/// does not hold changes nothing.
async fn revoke_user_role(
    client: &(impl GenericClient + Sync),
    project_env_key: &str,
    user_id: &str,
    role: &str,
    drops: &mut AuthDrops,
) -> Result<bool, AdministrationError> {
    let removed = client
        .execute(DROP_USER_ROLE_SQL, &[&user_id, &role])
        .await
        .map_err(|error| database_error(&error))?;
    drops.principal(project_env_key, user_id);
    Ok(removed == 1)
}

const USER_EXISTS_SQL: &str = "SELECT 1 FROM platform.\"user\" \
    WHERE id = $1::text::uuid AND type IN ('user', 'service')";

const USER_LIST_SQL: &str = "SELECT u.id::text, u.email, u.display_name, \
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

/// The application users of the database with their roles, by email.
pub async fn application_users(
    client: &(impl GenericClient + Sync),
) -> Result<Vec<ApplicationUserRow>, AdministrationError> {
    Ok(client
        .query(USER_LIST_SQL, &[])
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

/// The role names of the database, `admin` included.
pub async fn role_names(
    client: &(impl GenericClient + Sync),
) -> Result<Vec<String>, AdministrationError> {
    Ok(client
        .query(ROLE_LIST_SQL, &[])
        .await
        .map_err(|error| database_error(&error))?
        .iter()
        .map(|row| row.get(0))
        .collect())
}

/// The stored permissions of `role`.
pub async fn permission_rows(
    client: &(impl GenericClient + Sync),
    role: &str,
) -> Result<Vec<String>, AdministrationError> {
    require_role(client, role).await?;
    Ok(client
        .query(PERMISSION_LIST_SQL, &[&role])
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
    system: &tokio_postgres::Transaction<'_>,
    target: &(impl GenericClient + Sync),
    coordinate: ApplicationTarget<'_>,
    principal: &PrincipalId,
    drops: &mut AuthDrops,
) -> Result<(), IdentityError> {
    crate::org::lock_membership(system, principal, coordinate.org).await?;
    let row = system.query_opt(
        "SELECT p.type, p.status, p.email, p.display_name, p.subject, \
         EXISTS (SELECT 1 FROM identity.org_memberships m WHERE m.principal_id=p.id AND m.org=$2 AND m.status='active'), \
         identity.has_project_access(p.id,$2,$4,$3), \
         EXISTS (SELECT 1 FROM identity.project_roles r WHERE r.principal_id=p.id AND r.org=$2 AND r.project=$3 AND r.role='project-admin') \
         OR identity.is_project_admin(p.id,$2,$4,$3) \
         FROM identity.principals p WHERE p.id=$1::text::uuid",
        &[&principal.to_string().as_str(), &coordinate.org, &coordinate.project, &coordinate.env],
    ).await.map_err(|error| database_error(&error))?;
    let Some(row) = row else {
        return remove_user(target, coordinate.project_env_key, principal, drops).await;
    };
    let type_: String = row.get(0);
    let active = row.get::<_, String>(1) == "active";
    let admin: bool = row.get(7);
    if type_ == "service" {
        // Services derive administration from their project role, without user membership.
        let present = target
            .query_opt(
                "SELECT 1 FROM platform.\"user\" WHERE id=$1::text::uuid",
                &[&principal.to_string().as_str()],
            )
            .await
            .map_err(|error| database_error(&error))?
            .is_some();
        if active && admin && !present {
            let domain: String = system
                .query_one(
                    wamn_registry::sql::IDENTITY_PLATFORM_SRC_APPLICATION_PROJECT_AUTHORITY,
                    &[],
                )
                .await
                .map_err(|error| database_error(&error))?
                .get(0);
            let email = format!("{}@{domain}", row.get::<_, String>(4));
            write_service(
                target,
                coordinate.project_env_key,
                ApplicationUser {
                    principal_id: principal,
                    email: &email,
                    display_name: &row.get::<_, String>(3),
                },
                drops,
            )
            .await?;
        }
        if active && admin {
            target
                .execute(ROLE_SQL, &[&ADMIN_ROLE])
                .await
                .map_err(|error| database_error(&error))?;
            target
                .execute(
                    USER_ROLE_SQL,
                    &[&principal.to_string().as_str(), &ADMIN_ROLE],
                )
                .await
                .map_err(|error| database_error(&error))?;
            drops.principal(coordinate.project_env_key, principal.to_string().as_str());
        } else {
            remove_admin(target, coordinate.project_env_key, principal, drops).await?;
        }
        return Ok(());
    }
    if type_ != "user" || !active || !row.get::<_, bool>(5) || !row.get::<_, bool>(6) {
        return remove_user(target, coordinate.project_env_key, principal, drops).await;
    }
    let email: String = row.get(2);
    let display_name: String = row.get(3);
    let user = ApplicationUser {
        principal_id: principal,
        email: &email,
        display_name: &display_name,
    };
    write_user(target, coordinate.project_env_key, user, drops).await
}

/// The explicit environment of one authority projection.
#[derive(Debug, Clone, Copy)]
pub struct ApplicationTarget<'a> {
    pub org: &'a str,
    pub project: &'a str,
    pub env: &'a str,
    pub project_env_key: &'a str,
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
        user_id: &'a str,
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
    !wamn_catalog::is_host_route_operation(reference)
        || wamn_catalog::is_granted_host_route_operation(reference)
}

/// Mutate an authored grant inside the caller's application transaction.
/// The caller serializes role and permission writes with the application lock.
pub async fn mutate_grant(
    client: &(impl GenericClient + Sync),
    project_env_key: &str,
    authority: GrantAuthority<'_>,
    change: GrantChange<'_>,
    drops: &mut AuthDrops,
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
            let admin: bool = client.query_one(
                "SELECT EXISTS (SELECT 1 FROM platform.user_role WHERE user_id=$1::text::uuid AND role_name='admin')",
                &[&actor.to_string().as_str()],
            ).await.map_err(|error| database_error(&error))?.get(0);
            if !admin {
                let required = match change {
                    GrantChange::Permission { reference, .. } => vec![reference.to_owned()],
                    GrantChange::UserRole { role, .. } => permission_rows(client, role).await?,
                };
                for reference in required {
                    let held: bool = client.query_one(
                        "SELECT EXISTS (SELECT 1 FROM platform.user_role r JOIN platform.permission p ON p.role_name=r.role_name \
                         WHERE r.user_id=$1::text::uuid AND p.permission=$2)",
                        &[&actor.to_string().as_str(), &reference],
                    ).await.map_err(|error| database_error(&error))?.get(0);
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
                grant_user_role(client, project_env_key, user_id, role, drops).await?
            } else {
                revoke_user_role(client, project_env_key, user_id, role, drops).await?
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
            grant_permission(client, project_env_key, role, reference, drops)
                .await?
                .rows_added
        }
        GrantChange::Permission {
            role,
            reference,
            grant: false,
        } => {
            revoke_permission(client, project_env_key, role, reference, drops).await?;
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
    project_env_key: String,
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
