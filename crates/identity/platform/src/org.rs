//! Org membership and the administrative grants of an org
//! (docs/plan/platform-ui.md §4.4 and §4.5).
//!
//! Each grant here is the one write of its row. `wamn-ctl invite`, the
//! provisioning of an org owner and the control routes call these functions,
//! inside the caller's transaction and under the caller's bound actor.
//! Application `admin` rows follow in issue 4.

use tokio_postgres::GenericClient;

use crate::{
    IdentityError, IdentityErrorType, PrincipalId, checked_scope_segment, control, database_error,
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
        return Err(IdentityError::new(
            IdentityErrorType::NotFound,
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
        return Err(IdentityError::new(
            IdentityErrorType::NotFound,
            format!("principal {principal_id} is not an active member of org {org}"),
        ));
    }
    Ok(org)
}
