//! Org membership and the administrative grants of an org
//! (docs/architecture/overview.md#organization-membership-and-administration).
//!
//! Each grant here is the one write of its row. Platform installation
//! and the control routes call these functions,
//! inside the caller's transaction and under the caller's bound actor.
//! These functions write system rows only. The application rows of the same
//! grants are the functions of [`crate::identity::application`].

use sqlx::Row as _;
use sqlx::types::Uuid;
use wamn_identity_protocol::records::{EnvironmentSummary, MemberGrants, OrgUser, ProjectMember};

use super::control;
use crate::identity_error::database_error;
use wamn_authentication::control_admission::{ORG_ADMIN_ROLE, PROJECT_ADMIN_ROLE};
use wamn_authentication::identity_error::{IdentityError, IdentityErrorType, IdentityRefusal};
use wamn_identity_protocol::PrincipalId;

/// Create the active membership, or make an inactive membership active.
const ACTIVATE_MEMBERSHIP_SQL: &str = "INSERT INTO identity.org_memberships \
    (principal_id, org, status) VALUES ($1, $2, 'active') \
    ON CONFLICT (principal_id, org) DO UPDATE SET status = 'active' \
    WHERE identity.org_memberships.status <> 'active'";

const ACTIVE_MEMBER_SQL: &str = "SELECT status FROM identity.org_memberships \
    WHERE principal_id = $1 AND org = $2 FOR UPDATE";

const PROJECT_IN_ORG_SQL: &str = crate::identity_sql::PROJECT_IN_ORG_SQL;

const ORG_ROLE_SQL: &str = "INSERT INTO identity.org_roles (principal_id, org, role) \
    VALUES ($1, $2, $3) ON CONFLICT DO NOTHING";

fn membership_write_error(error: &sqlx::Error) -> IdentityError {
    if error.as_database_error().is_some_and(|error| {
        error.code().as_deref() == Some("23514")
            && error.constraint() == Some("org_memberships_single_org")
    }) {
        return IdentityError::refused(
            IdentityErrorType::Conflict,
            IdentityRefusal::UserInAnotherOrg,
            "the user already belongs to another org",
        );
    }
    database_error(error)
}

/// Create the user's active membership of `org`, or make an inactive
/// membership active. It restores no project, environment or role access.
pub async fn activate_org_membership(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &wamn_registry::OrgId,
) -> Result<(), IdentityError> {
    sqlx::query(ACTIVATE_MEMBERSHIP_SQL)
        .bind(Uuid::from(*principal_id))
        .bind(org.as_str())
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| membership_write_error(&error))?;
    Ok(())
}

/// Write the explicit org administration grant. Lower authority is derived.
pub async fn grant_org_admin(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &wamn_registry::OrgId,
) -> Result<(), IdentityError> {
    active_member(client, principal_id, org).await?;
    let principal = principal_id;
    sqlx::query(ORG_ROLE_SQL)
        .bind(Uuid::from(*principal))
        .bind(org.as_str())
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
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
) -> Result<(), IdentityError> {
    active_member(client, principal_id, org).await?;
    let in_org: bool = sqlx::query(PROJECT_IN_ORG_SQL)
        .bind(org.as_str())
        .bind(project.as_str())
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
    super::write_project_role(client, principal_id, org, project, PROJECT_ADMIN_ROLE).await
}

const ENV_IN_PROJECT_SQL: &str = crate::identity_sql::ENV_IN_PROJECT_SQL;

/// Grant a membership of one environment of the org to an active org member.
/// An environment outside the org is refused.
pub async fn grant_member(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
) -> Result<(), IdentityError> {
    active_member(client, principal_id, org).await?;
    let found: bool = sqlx::query(ENV_IN_PROJECT_SQL)
        .bind(org.as_str())
        .bind(project.as_str())
        .bind(env.as_str())
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
    super::grant_project_env_membership(client, principal_id, org, project, env).await
}

const HOLDS_ORG_ADMIN_SQL: &str = "SELECT EXISTS (SELECT 1 FROM identity.org_roles \
    WHERE principal_id = $1 AND org = $2 AND role = $3)";

/// Revoke a membership of one environment. The user's org membership and
/// other environments stay. While `org-admin` or `project-admin` covers the
/// environment, the revoke is refused: the covering grant goes first.
pub async fn revoke_member(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
) -> Result<(), IdentityError> {
    lock_membership(client, principal_id, org).await?;
    refuse_under_org_admin(client, principal_id, org).await?;
    let covered = control::is_project_admin(client, principal_id, org, project).await?;
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
    super::revoke_project_env_membership(client, principal_id, org, project, env).await?;
    Ok(())
}

const DROP_PROJECT_ADMIN_SQL: &str = "DELETE FROM identity.project_roles \
    WHERE principal_id = $1 AND org = $2 AND project = $3 AND role = $4";

/// Revoke `project-admin` in one project. Environment memberships stay.
/// While the user holds `org-admin`, the revoke is refused.
pub async fn revoke_project_admin(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
) -> Result<(), IdentityError> {
    lock_membership(client, principal_id, org).await?;
    refuse_under_org_admin(client, principal_id, org).await?;
    sqlx::query(DROP_PROJECT_ADMIN_SQL)
        .bind(Uuid::from(*principal_id))
        .bind(org.as_str())
        .bind(project.as_str())
        .bind(PROJECT_ADMIN_ROLE)
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
    org: &wamn_registry::OrgId,
) -> Result<(), IdentityError> {
    let covered: bool = sqlx::query(HOLDS_ORG_ADMIN_SQL)
        .bind(Uuid::from(*principal_id))
        .bind(org.as_str())
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

/// Require active membership of the organization.
pub(crate) async fn active_member(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &wamn_registry::OrgId,
) -> Result<(), IdentityError> {
    let active = lock_membership(client, principal_id, org)
        .await?
        .is_some_and(|status| status == "active");
    if !active {
        return Err(IdentityError::refused(
            IdentityErrorType::NotFound,
            IdentityRefusal::UserNotActive,
            format!("principal {principal_id} is not an active member of org {org}"),
        ));
    }
    Ok(())
}

const REACTIVATE_MEMBERSHIP_SQL: &str = "UPDATE identity.org_memberships SET status = 'active' \
    WHERE principal_id = $1 AND org = $2 AND status <> 'active'";

const DROP_ENV_MEMBERSHIPS_SQL: &str = "DELETE FROM identity.project_env_memberships \
    WHERE principal_id = $1 AND org = $2";

/// Every project role in the org, or only `$3` when it names one.
const DROP_PROJECT_ROLES_SQL: &str = "DELETE FROM identity.project_roles \
    WHERE principal_id = $1 AND org = $2 AND ($3::text IS NULL OR role = $3)";

/// Every org role in the org, or only `$3` when it names one.
const DROP_ORG_ROLES_SQL: &str = "DELETE FROM identity.org_roles \
    WHERE principal_id = $1 AND org = $2 AND ($3::text IS NULL OR role = $3)";

const DEACTIVATE_MEMBERSHIP_SQL: &str = "UPDATE identity.org_memberships SET status = 'inactive' \
    WHERE principal_id = $1 AND org = $2 AND status <> 'inactive'";

const ORG_USERS_SQL: &str = "SELECT p.id, p.email, p.display_name, m.status, p.status, \
    EXISTS (SELECT 1 FROM identity.org_roles o \
        WHERE o.principal_id = p.id AND o.org = $1 AND o.role = $2) \
    FROM identity.org_memberships m JOIN identity.principals p ON p.id = m.principal_id \
    WHERE m.org = $1 AND ($3::text IS NULL OR (p.email, p.id) > ($3, $4)) \
    ORDER BY p.email, p.id LIMIT $5";

/// Write the active org membership of an invited user, then each requested
/// grant through its one write. Control also uses this for creation without grants.
pub async fn invite_member(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &wamn_registry::OrgId,
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

/// Disable a user of this org if no other org has an active membership.
///
/// The issuer calls this within a read-committed write transaction. The
/// principal lock orders the membership check against concurrent activations
/// and the status change against renewal.
pub async fn disable_user(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    principal: &PrincipalId,
    org: &wamn_registry::OrgId,
) -> Result<(), IdentityError> {
    let user = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM identity.principals WHERE id = $1 AND type = 'user' FOR NO KEY UPDATE",
    )
    .bind(Uuid::from(*principal))
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| database_error(&error))?;
    let (member, active_elsewhere): (bool, bool) = sqlx::query_as(
        "SELECT EXISTS (SELECT FROM identity.org_memberships WHERE principal_id = $1 AND org = $2),
                EXISTS (SELECT FROM identity.org_memberships
                        WHERE principal_id = $1 AND org <> $2 AND status = 'active')",
    )
    .bind(Uuid::from(*principal))
    .bind(org.as_str())
    .fetch_one(&mut **tx)
    .await
    .map_err(|error| database_error(&error))?;
    if user.is_none() || !member {
        return Err(IdentityError::refused(
            IdentityErrorType::NotFound,
            IdentityRefusal::UserNotFound,
            "the org has no such user",
        ));
    }
    if active_elsewhere {
        return Err(IdentityError::refused(
            IdentityErrorType::Conflict,
            IdentityRefusal::UserInAnotherOrg,
            "the user has an active membership in another org",
        ));
    }
    super::disable_principal(tx, principal).await?;
    Ok(())
}

/// Make an existing org membership active. It restores no project,
/// environment or role access. A principal that is not a member is refused.
pub async fn reactivate_org_membership(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &wamn_registry::OrgId,
) -> Result<(), IdentityError> {
    member(client, principal_id, org).await?;
    sqlx::query(REACTIVATE_MEMBERSHIP_SQL)
        .bind(Uuid::from(*principal_id))
        .bind(org.as_str())
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| membership_write_error(&error))?;
    Ok(())
}

/// Revoke from the leaves upward: the environment memberships, the project
/// roles and the org roles of the user in the org, then mark the membership
/// inactive. The global principal stays as it is. A principal that is not a
/// member is refused.
pub async fn deactivate_org_membership(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &wamn_registry::OrgId,
) -> Result<(), IdentityError> {
    member(client, principal_id, org).await?;
    let principal = principal_id;
    let every: Option<&str> = None;
    sqlx::query(DROP_ENV_MEMBERSHIPS_SQL)
        .bind(Uuid::from(*principal))
        .bind(org.as_str())
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    sqlx::query(DROP_PROJECT_ROLES_SQL)
        .bind(Uuid::from(*principal))
        .bind(org.as_str())
        .bind(every)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    sqlx::query(DROP_ORG_ROLES_SQL)
        .bind(Uuid::from(*principal))
        .bind(org.as_str())
        .bind(every)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    sqlx::query(DEACTIVATE_MEMBERSHIP_SQL)
        .bind(Uuid::from(*principal))
        .bind(org.as_str())
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
    org: &wamn_registry::OrgId,
) -> Result<(), IdentityError> {
    lock_membership(client, principal_id, org).await?;
    let principal = principal_id;
    sqlx::query(DROP_ORG_ROLES_SQL)
        .bind(Uuid::from(*principal))
        .bind(org.as_str())
        .bind(Some(ORG_ADMIN_ROLE))
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    Ok(())
}

/// The active and inactive members of `org`, by email.
pub async fn org_users(
    client: &mut sqlx::PgConnection,
    org: &wamn_registry::OrgId,
) -> Result<Vec<OrgUser>, IdentityError> {
    org_users_page(client, org, None, None, None).await
}

/// Read a keyset page, or all rows when the limit is absent.
pub async fn org_users_page(
    client: &mut sqlx::PgConnection,
    org: &wamn_registry::OrgId,
    after: Option<&str>,
    after_id: Option<&PrincipalId>,
    limit: Option<i64>,
) -> Result<Vec<OrgUser>, IdentityError> {
    sqlx::query(ORG_USERS_SQL)
        .bind(org.as_str())
        .bind(ORG_ADMIN_ROLE)
        .bind(after)
        .bind(after_id.copied().map(Uuid::from))
        .bind(limit)
        .fetch_all(&mut *client)
        .await
        .map_err(|error| database_error(&error))?
        .iter()
        .map(|row| {
            Ok(OrgUser {
                principal_id: PrincipalId::from(
                    row.try_get::<Uuid, _>(0)
                        .map_err(|error| database_error(&error))?,
                ),
                email: row.try_get(1).map_err(|error| database_error(&error))?,
                display_name: row.try_get(2).map_err(|error| database_error(&error))?,
                status: row.try_get(3).map_err(|error| database_error(&error))?,
                principal_status: row.try_get(4).map_err(|error| database_error(&error))?,
                org_admin: row.try_get(5).map_err(|error| database_error(&error))?,
            })
        })
        .collect()
}

const USER_CONTACT_SQL: &str = "SELECT email, display_name FROM identity.principals \
    WHERE id = $1 AND type = 'user'";

/// The email and display name of a user principal, which its application
/// rows carry.
pub async fn user_contact(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
) -> Result<(String, String), IdentityError> {
    let row = sqlx::query(USER_CONTACT_SQL)
        .bind(Uuid::from(*principal_id))
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

const PROJECT_ENVS_SQL: &str = crate::identity_sql::PROJECT_ENVS_SQL;

/// The environments of one project of `org`.
pub async fn project_envs(
    client: &mut sqlx::PgConnection,
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
) -> Result<Vec<EnvironmentSummary>, IdentityError> {
    project_envs_page(client, org, project, None, None).await
}

/// Read a keyset page, or all rows when the limit is absent.
pub async fn project_envs_page(
    client: &mut sqlx::PgConnection,
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    after: Option<&str>,
    limit: Option<i64>,
) -> Result<Vec<EnvironmentSummary>, IdentityError> {
    sqlx::query(PROJECT_ENVS_SQL)
        .bind(org.as_str())
        .bind(project.as_str())
        .bind(after)
        .bind(limit)
        .fetch_all(&mut *client)
        .await
        .map_err(|error| database_error(&error))?
        .iter()
        .map(|row| {
            Ok(EnvironmentSummary {
                name: row
                    .try_get::<String, _>(0)
                    .and_then(|value| crate::decode_text(value, 0))
                    .map_err(|error| database_error(&error))?,
                state: row.get(1),
            })
        })
        .collect::<Result<_, IdentityError>>()
}

/// Every user with a membership or a project role in the project, with the
/// administrative grants that cover the project. An environment `$8` keeps
/// only its members and only its name in `environments`.
const PROJECT_MEMBERS_SQL: &str = crate::identity_sql::PROJECT_MEMBERS_SQL;

/// The users of one project of `org`, by email.
pub async fn project_members(
    client: &mut sqlx::PgConnection,
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
) -> Result<Vec<ProjectMember>, IdentityError> {
    project_members_page(client, org, project, None, None, None, None).await
}

/// Read a keyset page, or all rows when the limit is absent. An environment
/// keeps only its members.
pub async fn project_members_page(
    client: &mut sqlx::PgConnection,
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    environment: Option<&wamn_registry::EnvName>,
    after: Option<&str>,
    after_id: Option<&PrincipalId>,
    limit: Option<i64>,
) -> Result<Vec<ProjectMember>, IdentityError> {
    sqlx::query(PROJECT_MEMBERS_SQL)
        .bind(org.as_str())
        .bind(project.as_str())
        .bind(PROJECT_ADMIN_ROLE)
        .bind(ORG_ADMIN_ROLE)
        .bind(after)
        .bind(after_id.copied().map(Uuid::from))
        .bind(limit)
        .bind(environment.map(wamn_registry::EnvName::as_str))
        .fetch_all(&mut *client)
        .await
        .map_err(|error| database_error(&error))?
        .iter()
        .map(|row| {
            Ok(ProjectMember {
                principal_id: PrincipalId::from(
                    row.try_get::<Uuid, _>(0)
                        .map_err(|error| database_error(&error))?,
                ),
                email: row.try_get(1).map_err(|error| database_error(&error))?,
                display_name: row.try_get(2).map_err(|error| database_error(&error))?,
                org_admin: row.try_get(3).map_err(|error| database_error(&error))?,
                project_admin: row.try_get(4).map_err(|error| database_error(&error))?,
                environments: row
                    .try_get::<Vec<String>, _>(5)
                    .and_then(|names| {
                        names
                            .into_iter()
                            .map(|name| crate::decode_text(name, 5))
                            .collect()
                    })
                    .map_err(|error| database_error(&error))?,
            })
        })
        .collect()
}

/// Require membership of the organization, active or not.
async fn member(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &wamn_registry::OrgId,
) -> Result<(), IdentityError> {
    let found = lock_membership(client, principal_id, org).await?.is_some();
    if !found {
        return Err(IdentityError::refused(
            IdentityErrorType::NotFound,
            IdentityRefusal::UserNotFound,
            format!("principal {principal_id} is not a member of org {org}"),
        ));
    }
    Ok(())
}

/// Lock a user's org membership until the caller's system transaction ends.
/// An absent row grants no authority. Services have no org membership.
pub async fn lock_membership(
    client: &mut sqlx::PgConnection,
    principal: &PrincipalId,
    org: &wamn_registry::OrgId,
) -> Result<Option<String>, IdentityError> {
    sqlx::query(ACTIVE_MEMBER_SQL)
        .bind(Uuid::from(*principal))
        .bind(org.as_str())
        .fetch_optional(&mut *client)
        .await
        .map(|row| row.map(|row| row.get(0)))
        .map_err(|error| database_error(&error))
}

/// Lock existing memberships in principal order before bulk authority writes.
pub async fn lock_org_memberships(
    client: &mut sqlx::PgConnection,
    org: &wamn_registry::OrgId,
) -> Result<(), IdentityError> {
    sqlx::query("SELECT principal_id FROM identity.org_memberships WHERE org=$1 ORDER BY principal_id FOR UPDATE").bind(org.as_str()).fetch_all(&mut *client)
        .await.map_err(|error| database_error(&error))?;
    Ok(())
}
