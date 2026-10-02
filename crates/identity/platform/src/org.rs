//! Org membership and the administrative grants of an org
//! (docs/plan/platform-ui.md §4.4 and §4.5).
//!
//! Each grant here is the one write of its row. `wamn-ctl invite`, the
//! provisioning of an org owner and the control routes call these functions,
//! inside the caller's transaction and under the caller's bound actor.
//! These functions write system rows only. The application rows of the same
//! grants are the functions of [`crate::application`].

use tokio_postgres::GenericClient;

use crate::{
    IdentityError, IdentityErrorType, IdentityRefusal, PrincipalId, checked_scope_segment, control,
    database_error,
};

/// The org role that holds every project and environment of its org.
pub const ORG_ADMIN_ROLE: &str = "org-admin";

/// Create the active membership, or make an inactive membership active.
const ACTIVATE_MEMBERSHIP_SQL: &str = "INSERT INTO identity.org_memberships \
    (principal_id, org, status) VALUES ($1::text::uuid, $2, 'active') \
    ON CONFLICT (principal_id, org) DO UPDATE SET status = 'active' \
    WHERE identity.org_memberships.status <> 'active'";

const ACTIVE_MEMBER_SQL: &str = "SELECT EXISTS (SELECT 1 FROM identity.org_memberships \
    WHERE principal_id = $1::text::uuid AND org = $2 AND status = 'active')";

const PROJECT_IN_ORG_SQL: &str =
    "SELECT EXISTS (SELECT 1 FROM registry.projects WHERE org = $1 AND id = $2)";

const ORG_ROLE_SQL: &str = "INSERT INTO identity.org_roles (principal_id, org, role) \
    VALUES ($1::text::uuid, $2, $3) ON CONFLICT DO NOTHING";

/// `project-admin` in every project of the org, or of one project when `$4`
/// names it.
const PROJECT_ADMIN_SQL: &str = "INSERT INTO identity.project_roles \
    (principal_id, org, project, role) \
    SELECT $1::text::uuid, p.org, p.id, $3 FROM registry.projects p \
    WHERE p.org = $2 AND ($4::text IS NULL OR p.id = $4) \
    ON CONFLICT DO NOTHING";

/// A membership in every environment of the org, or of one project when `$3`
/// names it.
const ENV_MEMBERSHIPS_SQL: &str = "INSERT INTO identity.project_env_memberships \
    (principal_id, org, project, env) \
    SELECT $1::text::uuid, e.org, e.project, e.env FROM registry.project_envs e \
    WHERE e.org = $2 AND ($3::text IS NULL OR e.project = $3) \
    ON CONFLICT DO NOTHING";

/// Create the user's active membership of `org`, or make an inactive
/// membership active. It restores no project, environment or role access.
pub async fn activate_org_membership(
    client: &(impl GenericClient + Sync),
    principal_id: &PrincipalId,
    org: &str,
) -> Result<(), IdentityError> {
    let org = checked_scope_segment("org", org)?;
    client
        .execute(ACTIVATE_MEMBERSHIP_SQL, &[&principal_id.as_str(), &org])
        .await
        .map_err(|error| database_error(&error))?;
    Ok(())
}

/// Write the `org-admin` row of an active org member, then `project-admin` in
/// every project of the org and a membership in every environment of the
/// org. Rows that exist stay as they are.
pub async fn grant_org_admin(
    client: &(impl GenericClient + Sync),
    principal_id: &PrincipalId,
    org: &str,
) -> Result<(), IdentityError> {
    let org = active_member(client, principal_id, org).await?;
    let principal = principal_id.as_str();
    let all: Option<&str> = None;
    client
        .execute(ORG_ROLE_SQL, &[&principal, &org, &ORG_ADMIN_ROLE])
        .await
        .map_err(|error| database_error(&error))?;
    client
        .execute(
            PROJECT_ADMIN_SQL,
            &[&principal, &org, &control::PROJECT_ADMIN_ROLE, &all],
        )
        .await
        .map_err(|error| database_error(&error))?;
    client
        .execute(ENV_MEMBERSHIPS_SQL, &[&principal, &org, &all])
        .await
        .map_err(|error| database_error(&error))?;
    Ok(())
}

/// Write the `project-admin` row of an active org member in one project of
/// the org, then a membership in every environment of that project. A
/// project outside the org is refused.
pub async fn grant_project_admin(
    client: &(impl GenericClient + Sync),
    principal_id: &PrincipalId,
    org: &str,
    project: &str,
) -> Result<(), IdentityError> {
    let org = active_member(client, principal_id, org).await?;
    let project = checked_scope_segment("project", project)?;
    let in_org: bool = client
        .query_one(PROJECT_IN_ORG_SQL, &[&org, &project])
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
    let principal = principal_id.as_str();
    let one = Some(project.as_str());
    client
        .execute(
            PROJECT_ADMIN_SQL,
            &[&principal, &org, &control::PROJECT_ADMIN_ROLE, &one],
        )
        .await
        .map_err(|error| database_error(&error))?;
    client
        .execute(ENV_MEMBERSHIPS_SQL, &[&principal, &org, &one])
        .await
        .map_err(|error| database_error(&error))?;
    Ok(())
}

const ENV_IN_PROJECT_SQL: &str = "SELECT EXISTS (SELECT 1 FROM registry.project_envs \
    WHERE org = $1 AND project = $2 AND env = $3)";

/// Grant a membership of one environment of the org to an active org member.
/// An environment outside the org is refused.
pub async fn grant_member(
    client: &(impl GenericClient + Sync),
    principal_id: &PrincipalId,
    org: &str,
    project: &str,
    env: &str,
) -> Result<(), IdentityError> {
    let org = active_member(client, principal_id, org).await?;
    let project = checked_scope_segment("project", project)?;
    let env = checked_scope_segment("env", env)?;
    let found: bool = client
        .query_one(ENV_IN_PROJECT_SQL, &[&org, &project, &env])
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

const HOLDS_PROJECT_ADMIN_SQL: &str = "SELECT EXISTS (SELECT 1 FROM identity.project_roles \
    WHERE principal_id = $1::text::uuid AND org = $2 AND project = $3 AND role = $4)";

/// Revoke a membership of one environment. The user's org membership and
/// other environments stay. While `org-admin` or `project-admin` covers the
/// environment, the revoke is refused: the covering grant goes first.
pub async fn revoke_member(
    client: &(impl GenericClient + Sync),
    principal_id: &PrincipalId,
    org: &str,
    project: &str,
    env: &str,
) -> Result<(), IdentityError> {
    let org = checked_scope_segment("org", org)?;
    let project = checked_scope_segment("project", project)?;
    refuse_under_org_admin(client, principal_id, &org).await?;
    let covered: bool = client
        .query_one(
            HOLDS_PROJECT_ADMIN_SQL,
            &[
                &principal_id.as_str(),
                &org,
                &project,
                &control::PROJECT_ADMIN_ROLE,
            ],
        )
        .await
        .map_err(|error| database_error(&error))?
        .get(0);
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
    crate::revoke_project_env_membership(client, principal_id, &org, &project, env).await?;
    Ok(())
}

const DROP_PROJECT_ADMIN_SQL: &str = "DELETE FROM identity.project_roles \
    WHERE principal_id = $1::text::uuid AND org = $2 AND project = $3 AND role = $4";

/// Revoke `project-admin` in one project. Environment memberships stay.
/// While the user holds `org-admin`, the revoke is refused.
pub async fn revoke_project_admin(
    client: &(impl GenericClient + Sync),
    principal_id: &PrincipalId,
    org: &str,
    project: &str,
) -> Result<(), IdentityError> {
    let org = checked_scope_segment("org", org)?;
    let project = checked_scope_segment("project", project)?;
    refuse_under_org_admin(client, principal_id, &org).await?;
    client
        .execute(
            DROP_PROJECT_ADMIN_SQL,
            &[
                &principal_id.as_str(),
                &org,
                &project,
                &control::PROJECT_ADMIN_ROLE,
            ],
        )
        .await
        .map_err(|error| database_error(&error))?;
    Ok(())
}

/// Refuse a project-level revoke while `org-admin` covers it.
async fn refuse_under_org_admin(
    client: &(impl GenericClient + Sync),
    principal_id: &PrincipalId,
    org: &str,
) -> Result<(), IdentityError> {
    let covered: bool = client
        .query_one(
            HOLDS_ORG_ADMIN_SQL,
            &[&principal_id.as_str(), &org, &ORG_ADMIN_ROLE],
        )
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

/// `project-admin` in the project for every holder of `org-admin` in the
/// org.
const ORG_ADMINS_PROJECT_ADMIN_SQL: &str = "INSERT INTO identity.project_roles \
    (principal_id, org, project, role) \
    SELECT r.principal_id, r.org, $2, $3 FROM identity.org_roles r \
    WHERE r.org = $1 AND r.role = $4 \
    ON CONFLICT DO NOTHING";

/// A membership in the environment for every holder of `project-admin` in
/// its project.
const PROJECT_ADMINS_MEMBERSHIP_SQL: &str = "INSERT INTO identity.project_env_memberships \
    (principal_id, org, project, env) \
    SELECT r.principal_id, r.org, r.project, $3 FROM identity.project_roles r \
    WHERE r.org = $1 AND r.project = $2 AND r.role = $4 \
    ON CONFLICT DO NOTHING";

/// Give a new project, or a new environment, the rows of the current
/// administrators: `project-admin` in the project for each `org-admin` of the
/// org, then a membership in the environment for each `project-admin` of the
/// project, which now includes every `org-admin`. Rows that exist stay as
/// they are, so provisioning the same environment again changes nothing.
pub async fn materialize_admin_grants(
    client: &(impl GenericClient + Sync),
    org: &str,
    project: &str,
    env: &str,
) -> Result<(), IdentityError> {
    let org = checked_scope_segment("org", org)?;
    let project = checked_scope_segment("project", project)?;
    let env = checked_scope_segment("env", env)?;
    client
        .execute(
            ORG_ADMINS_PROJECT_ADMIN_SQL,
            &[
                &org,
                &project,
                &control::PROJECT_ADMIN_ROLE,
                &ORG_ADMIN_ROLE,
            ],
        )
        .await
        .map_err(|error| database_error(&error))?;
    client
        .execute(
            PROJECT_ADMINS_MEMBERSHIP_SQL,
            &[&org, &project, &env, &control::PROJECT_ADMIN_ROLE],
        )
        .await
        .map_err(|error| database_error(&error))?;
    Ok(())
}

/// The checked org, when the principal is an active member of it.
async fn active_member(
    client: &(impl GenericClient + Sync),
    principal_id: &PrincipalId,
    org: &str,
) -> Result<String, IdentityError> {
    let org = checked_scope_segment("org", org)?;
    let active: bool = client
        .query_one(ACTIVE_MEMBER_SQL, &[&principal_id.as_str(), &org])
        .await
        .map_err(|error| database_error(&error))?
        .get(0);
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

const MEMBER_SQL: &str = "SELECT EXISTS (SELECT 1 FROM identity.org_memberships \
    WHERE principal_id = $1::text::uuid AND org = $2)";

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
    WHERE m.org = $1 ORDER BY p.email";

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
    client: &(impl GenericClient + Sync),
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
    client: &(impl GenericClient + Sync),
    principal_id: &PrincipalId,
    org: &str,
) -> Result<(), IdentityError> {
    let org = member(client, principal_id, org).await?;
    client
        .execute(REACTIVATE_MEMBERSHIP_SQL, &[&principal_id.as_str(), &org])
        .await
        .map_err(|error| database_error(&error))?;
    Ok(())
}

/// Revoke from the leaves upward: the environment memberships, the project
/// roles and the org roles of the user in the org, then mark the membership
/// inactive. The global principal stays as it is. A principal that is not a
/// member is refused.
pub async fn deactivate_org_membership(
    client: &(impl GenericClient + Sync),
    principal_id: &PrincipalId,
    org: &str,
) -> Result<(), IdentityError> {
    let org = member(client, principal_id, org).await?;
    let principal = principal_id.as_str();
    let every: Option<&str> = None;
    client
        .execute(DROP_ENV_MEMBERSHIPS_SQL, &[&principal, &org])
        .await
        .map_err(|error| database_error(&error))?;
    client
        .execute(DROP_PROJECT_ROLES_SQL, &[&principal, &org, &every])
        .await
        .map_err(|error| database_error(&error))?;
    client
        .execute(DROP_ORG_ROLES_SQL, &[&principal, &org, &every])
        .await
        .map_err(|error| database_error(&error))?;
    client
        .execute(DEACTIVATE_MEMBERSHIP_SQL, &[&principal, &org])
        .await
        .map_err(|error| database_error(&error))?;
    Ok(())
}

/// Remove `project-admin` throughout the org, then the `org-admin` row.
/// Ordinary memberships stay.
pub async fn revoke_org_admin(
    client: &(impl GenericClient + Sync),
    principal_id: &PrincipalId,
    org: &str,
) -> Result<(), IdentityError> {
    let org = checked_scope_segment("org", org)?;
    let principal = principal_id.as_str();
    client
        .execute(
            DROP_PROJECT_ROLES_SQL,
            &[&principal, &org, &Some(control::PROJECT_ADMIN_ROLE)],
        )
        .await
        .map_err(|error| database_error(&error))?;
    client
        .execute(
            DROP_ORG_ROLES_SQL,
            &[&principal, &org, &Some(ORG_ADMIN_ROLE)],
        )
        .await
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
    client: &(impl GenericClient + Sync),
    org: &str,
) -> Result<Vec<OrgUser>, IdentityError> {
    let org = checked_scope_segment("org", org)?;
    client
        .query(ORG_USERS_SQL, &[&org, &ORG_ADMIN_ROLE])
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

const ORG_ENVIRONMENTS_SQL: &str = "SELECT project, env FROM registry.project_envs \
    WHERE org = $1 AND ($2::text IS NULL OR project = $2) ORDER BY project, env";

/// The environments of `org`, or of one project of it, as `(project, env)`.
pub async fn org_environments(
    client: &(impl GenericClient + Sync),
    org: &str,
    project: Option<&str>,
) -> Result<Vec<(String, String)>, IdentityError> {
    let org = checked_scope_segment("org", org)?;
    let project = project
        .map(|project| checked_scope_segment("project", project))
        .transpose()?;
    Ok(client
        .query(ORG_ENVIRONMENTS_SQL, &[&org, &project])
        .await
        .map_err(|error| database_error(&error))?
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect())
}

/// The status of a project environment (docs/plan/platform-ui.md §5.4).
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

/// The environments of one project of `org` that a status route changes:
/// one environment when `env` names it, or every environment of the project.
/// A project or an environment outside the org is refused before anything
/// is written.
pub async fn status_environments(
    client: &(impl GenericClient + Sync),
    org: &str,
    project: &str,
    env: Option<&str>,
) -> Result<Vec<String>, IdentityError> {
    let org = checked_scope_segment("org", org)?;
    let project = checked_scope_segment("project", project)?;
    let Some(env) = env else {
        let in_org: bool = client
            .query_one(PROJECT_IN_ORG_SQL, &[&org, &project])
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
        return project_envs(client, &org, &project).await;
    };
    let env = checked_scope_segment("env", env)?;
    let found: bool = client
        .query_one(ENV_IN_PROJECT_SQL, &[&org, &project, &env])
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

const ENVIRONMENT_STATUS_SQL: &str = "UPDATE registry.project_envs SET status = $4 \
    WHERE org = $1 AND project = $2 AND env = ANY($3)";

/// Write the status of the named environments of one project of `org`.
/// The control routes read them with [`status_environments`] first.
pub async fn set_environment_status(
    client: &(impl GenericClient + Sync),
    org: &str,
    project: &str,
    environments: &[String],
    status: EnvironmentStatus,
) -> Result<(), IdentityError> {
    let org = checked_scope_segment("org", org)?;
    let project = checked_scope_segment("project", project)?;
    client
        .execute(
            ENVIRONMENT_STATUS_SQL,
            &[&org, &project, &environments, &status.as_str()],
        )
        .await
        .map_err(|error| database_error(&error))?;
    Ok(())
}

const USER_CONTACT_SQL: &str = "SELECT email, display_name FROM identity.principals \
    WHERE id = $1::text::uuid AND type = 'user'";

/// The email and display name of a user principal, which its application
/// rows carry.
pub async fn user_contact(
    client: &(impl GenericClient + Sync),
    principal_id: &PrincipalId,
) -> Result<(String, String), IdentityError> {
    let row = client
        .query_opt(USER_CONTACT_SQL, &[&principal_id.as_str()])
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

const PROJECT_ENVS_SQL: &str =
    "SELECT env FROM registry.project_envs WHERE org = $1 AND project = $2 ORDER BY env";

/// The environments of one project of `org`.
pub async fn project_envs(
    client: &(impl GenericClient + Sync),
    org: &str,
    project: &str,
) -> Result<Vec<String>, IdentityError> {
    let org = checked_scope_segment("org", org)?;
    let project = checked_scope_segment("project", project)?;
    Ok(client
        .query(PROJECT_ENVS_SQL, &[&org, &project])
        .await
        .map_err(|error| database_error(&error))?
        .iter()
        .map(|row| row.get(0))
        .collect())
}

/// Every user with a membership or a project role in the project, with the
/// administrative grants that cover the project.
const PROJECT_MEMBERS_SQL: &str = "SELECT p.id::text, p.email, p.display_name, \
    EXISTS (SELECT 1 FROM identity.org_roles o \
        WHERE o.principal_id = p.id AND o.org = $1 AND o.role = $4), \
    EXISTS (SELECT 1 FROM identity.project_roles r \
        WHERE r.principal_id = p.id AND r.org = $1 AND r.project = $2 AND r.role = $3), \
    ARRAY(SELECT m.env FROM identity.project_env_memberships m \
        WHERE m.principal_id = p.id AND m.org = $1 AND m.project = $2 ORDER BY m.env) \
    FROM identity.principals p \
    WHERE EXISTS (SELECT 1 FROM identity.project_env_memberships m \
        WHERE m.principal_id = p.id AND m.org = $1 AND m.project = $2) \
    OR EXISTS (SELECT 1 FROM identity.project_roles r \
        WHERE r.principal_id = p.id AND r.org = $1 AND r.project = $2) \
    ORDER BY p.email";

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
    client: &(impl GenericClient + Sync),
    org: &str,
    project: &str,
) -> Result<Vec<ProjectMember>, IdentityError> {
    let org = checked_scope_segment("org", org)?;
    let project = checked_scope_segment("project", project)?;
    client
        .query(
            PROJECT_MEMBERS_SQL,
            &[
                &org,
                &project,
                &control::PROJECT_ADMIN_ROLE,
                &ORG_ADMIN_ROLE,
            ],
        )
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

/// The checked org, when the principal is a member of it, active or not.
async fn member(
    client: &(impl GenericClient + Sync),
    principal_id: &PrincipalId,
    org: &str,
) -> Result<String, IdentityError> {
    let org = checked_scope_segment("org", org)?;
    let found: bool = client
        .query_one(MEMBER_SQL, &[&principal_id.as_str(), &org])
        .await
        .map_err(|error| database_error(&error))?
        .get(0);
    if !found {
        return Err(IdentityError::refused(
            IdentityErrorType::NotFound,
            IdentityRefusal::UserNotFound,
            format!("principal {principal_id} is not a member of org {org}"),
        ));
    }
    Ok(org)
}
