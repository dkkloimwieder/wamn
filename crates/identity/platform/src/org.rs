//! Org membership and the administrative grants of an org
//! (docs/architecture/overview.md#organization-membership-and-administration).
//!
//! Each grant here is the one write of its row. `wamn-ctl invite`, the
//! provisioning of an org owner and the control routes call these functions,
//! inside the caller's transaction and under the caller's bound actor.
//! These functions write system rows only. The application rows of the same
//! grants are the functions of [`crate::application`].

use sqlx::Row as _;

use crate::{
    IdentityError, IdentityErrorType, IdentityRefusal, PrincipalId, control, database_error,
    require_target_segment,
};
use wamn_broker::auth_changes::AuthDrops;

/// The org role that holds every project and environment of its org.
pub const ORG_ADMIN_ROLE: &str = "org-admin";

/// Create the active membership, or make an inactive membership active.
const ACTIVATE_MEMBERSHIP_SQL: &str = "INSERT INTO identity.org_memberships \
    (principal_id, org, status) VALUES ($1::text::uuid, $2, 'active') \
    ON CONFLICT (principal_id, org) DO UPDATE SET status = 'active' \
    WHERE identity.org_memberships.status <> 'active'";

const ACTIVE_MEMBER_SQL: &str = "SELECT status FROM identity.org_memberships \
    WHERE principal_id = $1::text::uuid AND org = $2 FOR UPDATE";

const PROJECT_IN_ORG_SQL: &str = wamn_registry::sql::IDENTITY_PLATFORM_SRC_ORG_PROJECT_IN_ORG_SQL;

const ORG_ROLE_SQL: &str = "INSERT INTO identity.org_roles (principal_id, org, role) \
    VALUES ($1::text::uuid, $2, $3) ON CONFLICT DO NOTHING";

const PROJECT_ADMIN_SQL: &str = "INSERT INTO identity.project_roles \
    (principal_id, org, project, role) VALUES ($1::text::uuid, $2, $4, $3) ON CONFLICT DO NOTHING";

/// Create the user's active membership of `org`, or make an inactive
/// membership active. It restores no project, environment or role access.
pub async fn activate_org_membership(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &str,
) -> Result<(), IdentityError> {
    let org = require_target_segment("org", org)?;
    sqlx::query(ACTIVATE_MEMBERSHIP_SQL)
        .bind(principal_id.to_string().as_str())
        .bind(&org)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    Ok(())
}

/// Write the explicit org administration grant. Lower authority is derived.
pub async fn grant_org_admin(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &str,
) -> Result<(), IdentityError> {
    let org = active_member(client, principal_id, org).await?;
    let principal_text = principal_id.to_string();
    let principal = principal_text.as_str();
    sqlx::query(ORG_ROLE_SQL)
        .bind(principal)
        .bind(&org)
        .bind(ORG_ADMIN_ROLE)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    Ok(())
}

/// Write the explicit project administration grant of an active org member.
pub async fn grant_project_admin(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &str,
    project: &str,
) -> Result<(), IdentityError> {
    let org = active_member(client, principal_id, org).await?;
    let project = require_target_segment("project", project)?;
    let in_org: bool = sqlx::query(PROJECT_IN_ORG_SQL)
        .bind(&org)
        .bind(&project)
        .fetch_one(&mut *client)
        .await
        .map_err(|error| database_error(&error))?
        .get(0);
    if !in_org {
        return Err(IdentityError::refused(
            IdentityErrorType::NotFound,
            IdentityRefusal::ProjectNotFound,
            format!("project {project} is not a project of org {org}"),
        ));
    }
    let principal_text = principal_id.to_string();
    let principal = principal_text.as_str();
    let one = Some(project.as_str());
    sqlx::query(PROJECT_ADMIN_SQL)
        .bind(principal)
        .bind(&org)
        .bind(control::PROJECT_ADMIN_ROLE)
        .bind(one)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    Ok(())
}

const ENV_IN_PROJECT_SQL: &str = wamn_registry::sql::IDENTITY_PLATFORM_SRC_ORG_ENV_IN_PROJECT_SQL;

/// Grant a membership of one environment of the org to an active org member.
/// An environment outside the org is refused.
pub async fn grant_member(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &str,
    project: &str,
    env: &str,
) -> Result<(), IdentityError> {
    let org = active_member(client, principal_id, org).await?;
    let project = require_target_segment("project", project)?;
    let env = require_target_segment("env", env)?;
    let found: bool = sqlx::query(ENV_IN_PROJECT_SQL)
        .bind(&org)
        .bind(&project)
        .bind(&env)
        .fetch_one(&mut *client)
        .await
        .map_err(|error| database_error(&error))?
        .get(0);
    if !found {
        return Err(IdentityError::refused(
            IdentityErrorType::NotFound,
            IdentityRefusal::EnvironmentNotFound,
            format!("environment {project}/{env} is not an environment of org {org}"),
        ));
    }
    crate::grant_project_env_membership(client, principal_id, &org, &project, &env).await
}

const HOLDS_ORG_ADMIN_SQL: &str = "SELECT EXISTS (SELECT 1 FROM identity.org_roles \
    WHERE principal_id = $1::text::uuid AND org = $2 AND role = $3)";

/// Revoke a membership of one environment. The user's org membership and
/// other environments stay. While `org-admin` or `project-admin` covers the
/// environment, the revoke is refused: the covering grant goes first.
pub async fn revoke_member(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &str,
    project: &str,
    env: &str,
    drops: &mut AuthDrops,
) -> Result<(), IdentityError> {
    let org = require_target_segment("org", org)?;
    lock_membership(client, principal_id, &org).await?;
    let project = require_target_segment("project", project)?;
    refuse_under_org_admin(client, principal_id, &org).await?;
    let covered =
        control::is_project_admin(client, principal_id, &org, &project, Some(env)).await?;
    if covered {
        return Err(IdentityError::refused(
            IdentityErrorType::Conflict,
            IdentityRefusal::AdminCovered,
            format!(
                "principal {principal_id} holds project-admin in project {project}. \
                 Revoke project-admin first"
            ),
        ));
    }
    crate::revoke_project_env_membership(client, principal_id, &org, &project, env, drops).await?;
    Ok(())
}

const DROP_PROJECT_ADMIN_SQL: &str = "DELETE FROM identity.project_roles \
    WHERE principal_id = $1::text::uuid AND org = $2 AND project = $3 AND role = $4";

/// Revoke `project-admin` in one project. Environment memberships stay.
/// While the user holds `org-admin`, the revoke is refused.
pub async fn revoke_project_admin(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &str,
    project: &str,
    drops: &mut AuthDrops,
) -> Result<(), IdentityError> {
    let org = require_target_segment("org", org)?;
    lock_membership(client, principal_id, &org).await?;
    let project = require_target_segment("project", project)?;
    refuse_under_org_admin(client, principal_id, &org).await?;
    drops.identity(principal_id.to_string().as_str());
    sqlx::query(DROP_PROJECT_ADMIN_SQL)
        .bind(principal_id.to_string().as_str())
        .bind(&org)
        .bind(&project)
        .bind(control::PROJECT_ADMIN_ROLE)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    Ok(())
}

/// Refuse a project-level revoke while `org-admin` covers it.
async fn refuse_under_org_admin(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &str,
) -> Result<(), IdentityError> {
    let covered: bool = sqlx::query(HOLDS_ORG_ADMIN_SQL)
        .bind(principal_id.to_string().as_str())
        .bind(org)
        .bind(ORG_ADMIN_ROLE)
        .fetch_one(&mut *client)
        .await
        .map_err(|error| database_error(&error))?
        .get(0);
    if covered {
        return Err(IdentityError::refused(
            IdentityErrorType::Conflict,
            IdentityRefusal::AdminCovered,
            format!(
                "principal {principal_id} holds org-admin in org {org}. Revoke org-admin first"
            ),
        ));
    }
    Ok(())
}

const CREATE_PROJECT_SQL: &str = wamn_registry::sql::IDENTITY_PLATFORM_SRC_ORG_CREATE_PROJECT_SQL;

/// Create an org-level project definition. An existing project is refused.
pub async fn create_project(
    client: &mut sqlx::PgConnection,
    org: &str,
    project: &str,
) -> Result<(), IdentityError> {
    let org = require_target_segment("org", org)?;
    lock_org_memberships(client, &org).await?;
    let project = require_target_segment("project", project)?;
    let created = sqlx::query(CREATE_PROJECT_SQL)
        .bind(&org)
        .bind(&project)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    if created == 0 {
        return Err(IdentityError::refused(
            IdentityErrorType::Conflict,
            IdentityRefusal::ProjectExists,
            format!("project {project} exists in org {org}"),
        ));
    }
    Ok(())
}

/// The validated org, when the principal is an active member of it.
pub(crate) async fn active_member(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &str,
) -> Result<String, IdentityError> {
    let org = require_target_segment("org", org)?;
    let active = lock_membership(client, principal_id, &org)
        .await?
        .is_some_and(|status| status == "active");
    if !active {
        return Err(IdentityError::refused(
            IdentityErrorType::NotFound,
            IdentityRefusal::UserNotActive,
            format!("principal {principal_id} is not an active member of org {org}"),
        ));
    }
    Ok(org)
}

const REACTIVATE_MEMBERSHIP_SQL: &str = "UPDATE identity.org_memberships SET status = 'active' \
    WHERE principal_id = $1::text::uuid AND org = $2 AND status <> 'active'";

const DROP_ENV_MEMBERSHIPS_SQL: &str = "DELETE FROM identity.project_env_memberships \
    WHERE principal_id = $1::text::uuid AND org = $2";

/// Every project role in the org, or only `$3` when it names one.
const DROP_PROJECT_ROLES_SQL: &str = "DELETE FROM identity.project_roles \
    WHERE principal_id = $1::text::uuid AND org = $2 AND ($3::text IS NULL OR role = $3)";

/// Every org role in the org, or only `$3` when it names one.
const DROP_ORG_ROLES_SQL: &str = "DELETE FROM identity.org_roles \
    WHERE principal_id = $1::text::uuid AND org = $2 AND ($3::text IS NULL OR role = $3)";

const DEACTIVATE_MEMBERSHIP_SQL: &str = "UPDATE identity.org_memberships SET status = 'inactive' \
    WHERE principal_id = $1::text::uuid AND org = $2 AND status <> 'inactive'";

const ORG_USERS_SQL: &str = "SELECT p.id::text, p.email, p.display_name, m.status, \
    EXISTS (SELECT 1 FROM identity.org_roles o \
        WHERE o.principal_id = p.id AND o.org = $1 AND o.role = $2) \
    FROM identity.org_memberships m JOIN identity.principals p ON p.id = m.principal_id \
    WHERE m.org = $1 AND ($3::text IS NULL OR (p.email, p.id) > ($3, $4::text::uuid)) \
    ORDER BY p.email, p.id LIMIT $5";

/// The grants an invitation asks for, beside the org membership.
#[derive(Debug, Default)]
pub struct MemberGrants {
    /// Grant `org-admin` in the org.
    pub org_admin: bool,
    /// Projects of the org where the user gets `project-admin`.
    pub project_admins: Vec<String>,
    /// Environments of the org, as `(project, env)`, where the user gets a
    /// membership.
    pub memberships: Vec<(String, String)>,
}

/// Write the active org membership of an invited user, then each requested
/// grant through its one write. `wamn-ctl invite` and `user.invite` call it.
pub async fn invite_member(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &str,
    grants: &MemberGrants,
) -> Result<(), IdentityError> {
    activate_org_membership(client, principal_id, org).await?;
    if grants.org_admin {
        grant_org_admin(client, principal_id, org).await?;
    }
    for project in &grants.project_admins {
        grant_project_admin(client, principal_id, org, project).await?;
    }
    for (project, env) in &grants.memberships {
        grant_member(client, principal_id, org, project, env).await?;
    }
    Ok(())
}

/// Make an existing org membership active. It restores no project,
/// environment or role access. A principal that is not a member is refused.
pub async fn reactivate_org_membership(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &str,
) -> Result<(), IdentityError> {
    let org = member(client, principal_id, org).await?;
    sqlx::query(REACTIVATE_MEMBERSHIP_SQL)
        .bind(principal_id.to_string().as_str())
        .bind(&org)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    Ok(())
}

/// Revoke from the leaves upward: the environment memberships, the project
/// roles and the org roles of the user in the org, then mark the membership
/// inactive. The global principal stays as it is. A principal that is not a
/// member is refused.
pub async fn deactivate_org_membership(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &str,
    drops: &mut AuthDrops,
) -> Result<(), IdentityError> {
    let org = member(client, principal_id, org).await?;
    let principal_text = principal_id.to_string();
    let principal = principal_text.as_str();
    drops.identity(principal);
    let every: Option<&str> = None;
    sqlx::query(DROP_ENV_MEMBERSHIPS_SQL)
        .bind(principal)
        .bind(&org)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    sqlx::query(DROP_PROJECT_ROLES_SQL)
        .bind(principal)
        .bind(&org)
        .bind(every)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    sqlx::query(
        "DELETE FROM identity.environment_roles WHERE principal_id=$1::text::uuid AND org=$2",
    )
    .bind(principal)
    .bind(&org)
    .execute(&mut *client)
    .await
    .map(|result| result.rows_affected())
    .map_err(|error| database_error(&error))?;
    sqlx::query(DROP_ORG_ROLES_SQL)
        .bind(principal)
        .bind(&org)
        .bind(every)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    sqlx::query(DEACTIVATE_MEMBERSHIP_SQL)
        .bind(principal)
        .bind(&org)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    Ok(())
}

/// Revoke only the explicit org grant. Other explicit grants stay.
pub async fn revoke_org_admin(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &str,
    drops: &mut AuthDrops,
) -> Result<(), IdentityError> {
    let org = require_target_segment("org", org)?;
    lock_membership(client, principal_id, &org).await?;
    let principal_text = principal_id.to_string();
    let principal = principal_text.as_str();
    drops.identity(principal);
    sqlx::query(DROP_ORG_ROLES_SQL)
        .bind(principal)
        .bind(&org)
        .bind(Some(ORG_ADMIN_ROLE))
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    Ok(())
}

/// One member of an org.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct OrgUser {
    /// The user principal.
    pub principal_id: String,
    /// The user's email.
    pub email: String,
    /// The user's display name.
    pub display_name: String,
    /// The org-local status: `active` or `inactive`.
    pub status: String,
    /// Whether the user holds `org-admin` in the org.
    pub org_admin: bool,
}

/// The active and inactive members of `org`, by email.
pub async fn org_users(
    client: &mut sqlx::PgConnection,
    org: &str,
) -> Result<Vec<OrgUser>, IdentityError> {
    org_users_page(client, org, None, None, None).await
}

/// Read a keyset page, or all rows when the limit is absent.
pub async fn org_users_page(
    client: &mut sqlx::PgConnection,
    org: &str,
    after: Option<&str>,
    after_id: Option<&str>,
    limit: Option<i64>,
) -> Result<Vec<OrgUser>, IdentityError> {
    let org = require_target_segment("org", org)?;
    sqlx::query(ORG_USERS_SQL)
        .bind(&org)
        .bind(ORG_ADMIN_ROLE)
        .bind(after)
        .bind(after_id)
        .bind(limit)
        .fetch_all(&mut *client)
        .await
        .map_err(|error| database_error(&error))?
        .iter()
        .map(|row| {
            Ok(OrgUser {
                principal_id: row.try_get(0).map_err(|error| database_error(&error))?,
                email: row.try_get(1).map_err(|error| database_error(&error))?,
                display_name: row.try_get(2).map_err(|error| database_error(&error))?,
                status: row.try_get(3).map_err(|error| database_error(&error))?,
                org_admin: row.try_get(4).map_err(|error| database_error(&error))?,
            })
        })
        .collect()
}

const ORG_ENVIRONMENTS_SQL: &str =
    wamn_registry::sql::IDENTITY_PLATFORM_SRC_ORG_ORG_ENVIRONMENTS_SQL;

/// The environments of `org`, or of one project of it, as `(project, env)`.
pub async fn org_environments(
    client: &mut sqlx::PgConnection,
    org: &str,
    project: Option<&str>,
) -> Result<Vec<(String, String)>, IdentityError> {
    let org = require_target_segment("org", org)?;
    let project = project
        .map(|project| require_target_segment("project", project))
        .transpose()?;
    Ok(sqlx::query(ORG_ENVIRONMENTS_SQL)
        .bind(&org)
        .bind(&project)
        .fetch_all(&mut *client)
        .await
        .map_err(|error| database_error(&error))?
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect())
}

/// The status of a project environment
/// (docs/architecture/execution.md#host-run-routes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvironmentStatus {
    /// Identity offers its audience and its host serves its routes.
    Active,
    /// Identity offers no audience and its host answers
    /// `environment-inactive`. CDC, data and grants stay.
    Inactive,
}

impl EnvironmentStatus {
    /// The stored value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Inactive => "inactive",
        }
    }
}

/// An environment and its registry state.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct EnvironmentSummary {
    /// The environment name.
    pub name: String,
    /// The state from registry.environments.status.
    pub state: String,
}

/// The environments of one project of `org` that a status route changes:
/// one environment when `env` names it, or every environment of the project.
/// A project or an environment outside the org is refused before anything
/// is written.
pub async fn status_environments(
    client: &mut sqlx::PgConnection,
    org: &str,
    project: &str,
    env: Option<&str>,
) -> Result<Vec<String>, IdentityError> {
    let org = require_target_segment("org", org)?;
    let project = require_target_segment("project", project)?;
    let Some(env) = env else {
        let in_org: bool = sqlx::query(PROJECT_IN_ORG_SQL)
            .bind(&org)
            .bind(&project)
            .fetch_one(&mut *client)
            .await
            .map_err(|error| database_error(&error))?
            .get(0);
        if !in_org {
            return Err(IdentityError::refused(
                IdentityErrorType::NotFound,
                IdentityRefusal::ProjectNotFound,
                format!("project {project} is not a project of org {org}"),
            ));
        }
        return Ok(project_envs(client, &org, &project)
            .await?
            .into_iter()
            .map(|entry| entry.name)
            .collect());
    };
    let env = require_target_segment("env", env)?;
    let found: bool = sqlx::query(ENV_IN_PROJECT_SQL)
        .bind(&org)
        .bind(&project)
        .bind(&env)
        .fetch_one(&mut *client)
        .await
        .map_err(|error| database_error(&error))?
        .get(0);
    if !found {
        return Err(IdentityError::refused(
            IdentityErrorType::NotFound,
            IdentityRefusal::EnvironmentNotFound,
            format!("environment {project}/{env} is not an environment of org {org}"),
        ));
    }
    Ok(vec![env])
}

const ENVIRONMENT_STATUS_SQL: &str =
    wamn_registry::sql::IDENTITY_PLATFORM_SRC_ORG_ENVIRONMENT_STATUS_SQL;

/// Write the status of the named environments of one project of `org`.
/// The control routes read them with [`status_environments`] first.
pub async fn set_environment_status(
    client: &mut sqlx::PgConnection,
    org: &str,
    project: &str,
    environments: &[String],
    status: EnvironmentStatus,
) -> Result<(), IdentityError> {
    let org = require_target_segment("org", org)?;
    let project = require_target_segment("project", project)?;
    sqlx::query(ENVIRONMENT_STATUS_SQL)
        .bind(&org)
        .bind(&project)
        .bind(environments)
        .bind(status.as_str())
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    Ok(())
}

const USER_CONTACT_SQL: &str = "SELECT email, display_name FROM identity.principals \
    WHERE id = $1::text::uuid AND type = 'user'";

/// The email and display name of a user principal, which its application
/// rows carry.
pub async fn user_contact(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
) -> Result<(String, String), IdentityError> {
    let row = sqlx::query(USER_CONTACT_SQL)
        .bind(principal_id.to_string().as_str())
        .fetch_optional(&mut *client)
        .await
        .map_err(|error| database_error(&error))?
        .ok_or_else(|| {
            IdentityError::refused(
                IdentityErrorType::NotFound,
                IdentityRefusal::UserNotFound,
                format!("principal {principal_id} is not a user"),
            )
        })?;
    let email: Option<String> = row.get(0);
    let email = email.ok_or_else(|| {
        IdentityError::refused(
            IdentityErrorType::NotFound,
            IdentityRefusal::UserNotFound,
            format!("user principal {principal_id} has no email"),
        )
    })?;
    Ok((email, row.get(1)))
}

const PROJECT_ENVS_SQL: &str = wamn_registry::sql::IDENTITY_PLATFORM_SRC_ORG_PROJECT_ENVS_SQL;

/// The environments of one project of `org`.
pub async fn project_envs(
    client: &mut sqlx::PgConnection,
    org: &str,
    project: &str,
) -> Result<Vec<EnvironmentSummary>, IdentityError> {
    project_envs_page(client, org, project, None, None).await
}

/// Read a keyset page, or all rows when the limit is absent.
pub async fn project_envs_page(
    client: &mut sqlx::PgConnection,
    org: &str,
    project: &str,
    after: Option<&str>,
    limit: Option<i64>,
) -> Result<Vec<EnvironmentSummary>, IdentityError> {
    let org = require_target_segment("org", org)?;
    let project = require_target_segment("project", project)?;
    Ok(sqlx::query(PROJECT_ENVS_SQL)
        .bind(&org)
        .bind(&project)
        .bind(after)
        .bind(limit)
        .fetch_all(&mut *client)
        .await
        .map_err(|error| database_error(&error))?
        .iter()
        .map(|row| EnvironmentSummary {
            name: row.get(0),
            state: row.get(1),
        })
        .collect())
}

/// Every user with a membership or a project role in the project, with the
/// administrative grants that cover the project. An environment `$8` keeps
/// only its members and only its name in `environments`.
const PROJECT_MEMBERS_SQL: &str = wamn_registry::sql::IDENTITY_PLATFORM_SRC_ORG_PROJECT_MEMBERS_SQL;

/// One user of a project.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProjectMember {
    /// The user principal.
    pub principal_id: String,
    /// The user's email.
    pub email: String,
    /// The user's display name.
    pub display_name: String,
    /// Whether `org-admin` covers the project.
    pub org_admin: bool,
    /// Whether the user holds `project-admin` in the project.
    pub project_admin: bool,
    /// The environments of the project where the user is a member.
    pub environments: Vec<String>,
}

/// The users of one project of `org`, by email.
pub async fn project_members(
    client: &mut sqlx::PgConnection,
    org: &str,
    project: &str,
) -> Result<Vec<ProjectMember>, IdentityError> {
    project_members_page(client, org, project, None, None, None, None).await
}

/// Read a keyset page, or all rows when the limit is absent. An environment
/// keeps only its members.
pub async fn project_members_page(
    client: &mut sqlx::PgConnection,
    org: &str,
    project: &str,
    environment: Option<&str>,
    after: Option<&str>,
    after_id: Option<&str>,
    limit: Option<i64>,
) -> Result<Vec<ProjectMember>, IdentityError> {
    let org = require_target_segment("org", org)?;
    let project = require_target_segment("project", project)?;
    let environment = environment
        .map(|environment| require_target_segment("environment", environment))
        .transpose()?;
    sqlx::query(PROJECT_MEMBERS_SQL)
        .bind(&org)
        .bind(&project)
        .bind(control::PROJECT_ADMIN_ROLE)
        .bind(ORG_ADMIN_ROLE)
        .bind(after)
        .bind(after_id)
        .bind(limit)
        .bind(&environment)
        .fetch_all(&mut *client)
        .await
        .map_err(|error| database_error(&error))?
        .iter()
        .map(|row| {
            Ok(ProjectMember {
                principal_id: row.try_get(0).map_err(|error| database_error(&error))?,
                email: row.try_get(1).map_err(|error| database_error(&error))?,
                display_name: row.try_get(2).map_err(|error| database_error(&error))?,
                org_admin: row.try_get(3).map_err(|error| database_error(&error))?,
                project_admin: row.try_get(4).map_err(|error| database_error(&error))?,
                environments: row.try_get(5).map_err(|error| database_error(&error))?,
            })
        })
        .collect()
}

/// The validated org, when the principal is a member of it, active or not.
async fn member(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &str,
) -> Result<String, IdentityError> {
    let org = require_target_segment("org", org)?;
    let found = lock_membership(client, principal_id, &org).await?.is_some();
    if !found {
        return Err(IdentityError::refused(
            IdentityErrorType::NotFound,
            IdentityRefusal::UserNotFound,
            format!("principal {principal_id} is not a member of org {org}"),
        ));
    }
    Ok(org)
}

/// Lock a user's org membership until the caller's system transaction ends.
/// An absent row grants no authority. Services have no org membership.
pub async fn lock_membership(
    client: &mut sqlx::PgConnection,
    principal: &PrincipalId,
    org: &str,
) -> Result<Option<String>, IdentityError> {
    sqlx::query(ACTIVE_MEMBER_SQL)
        .bind(principal.to_string().as_str())
        .bind(org)
        .fetch_optional(&mut *client)
        .await
        .map(|row| row.map(|row| row.get(0)))
        .map_err(|error| database_error(&error))
}

/// Lock existing memberships in principal order before bulk authority writes.
pub async fn lock_org_memberships(
    client: &mut sqlx::PgConnection,
    org: &str,
) -> Result<(), IdentityError> {
    sqlx::query("SELECT principal_id FROM identity.org_memberships WHERE org=$1 ORDER BY principal_id FOR UPDATE").bind(org).fetch_all(&mut *client)
        .await.map_err(|error| database_error(&error))?;
    Ok(())
}

/// Grant explicit administration of one environment to an active org member.
pub async fn grant_environment_admin(
    client: &mut sqlx::PgConnection,
    principal: &PrincipalId,
    org: &str,
    env: &str,
) -> Result<(), IdentityError> {
    let org = active_member(client, principal, org).await?;
    let env = require_target_segment("environment", env)?;
    sqlx::query("INSERT INTO identity.environment_roles (principal_id,org,env,role) VALUES ($1::text::uuid,$2,$3,'environment-admin') ON CONFLICT DO NOTHING").bind(principal.to_string()).bind(&org).bind(&env).execute(&mut *client).await.map(|result| result.rows_affected()).map_err(|error| database_error(&error))?;
    Ok(())
}
/// Revoke only the explicit environment grant.
pub async fn revoke_environment_admin(
    client: &mut sqlx::PgConnection,
    principal: &PrincipalId,
    org: &str,
    env: &str,
    drops: &mut AuthDrops,
) -> Result<(), IdentityError> {
    let org = require_target_segment("org", org)?;
    let env = require_target_segment("environment", env)?;
    lock_membership(client, principal, &org).await?;
    sqlx::query("DELETE FROM identity.environment_roles WHERE principal_id=$1::text::uuid AND org=$2 AND env=$3").bind(principal.to_string()).bind(&org).bind(&env).execute(&mut *client).await.map(|result| result.rows_affected()).map_err(|error| database_error(&error))?;
    drops.identity(principal.to_string().as_str());
    Ok(())
}
