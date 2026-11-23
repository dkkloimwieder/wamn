//! Control authority: who may administer an org
//! (docs/architecture/execution.md#control-sessions).
//!
//! An org access token carries no application roles. The
//! org role `org-admin`, or the project role `project-admin` in a project of
//! the org, grants control authority in that org.

use sqlx::Row as _;
use sqlx::types::Uuid;

use crate::org::ORG_ADMIN_ROLE;
use crate::{IdentityError, PrincipalId, database_error};

/// The project role that grants control authority in its org.
pub const PROJECT_ADMIN_ROLE: &str = "project-admin";

const CONTROL_PROJECTS_SQL: &str =
    wamn_registry::sql::IDENTITY_PLATFORM_SRC_CONTROL_CONTROL_PROJECTS_SQL;

const ORG_PROJECTS_SQL: &str = wamn_registry::sql::IDENTITY_PLATFORM_SRC_CONTROL_ORG_PROJECTS_SQL;

const ORG_ADMIN_SQL: &str = "SELECT EXISTS (SELECT 1 FROM identity.org_roles r \
    JOIN identity.principals p ON p.id = r.principal_id \
    WHERE r.principal_id = $1 AND r.org = $2 AND r.role = $3 \
    AND p.type = 'user' AND p.status = 'active' \
    AND EXISTS (SELECT 1 FROM identity.org_memberships m WHERE m.principal_id=p.id AND m.org=$2 AND m.status='active'))";

const PROJECT_ADMIN_SQL: &str = "SELECT identity.is_project_admin($1,$2,$3) AND $4='project-admin'";

/// The org of an org audience, or `None` when the audience is not one.
pub fn org_audience_org(audience: &str) -> Option<wamn_registry::OrgId> {
    audience
        .strip_prefix(wamn_registry::name::ORG_AUDIENCE_PREFIX)?
        .parse()
        .ok()
}

/// The projects of `org` where the active user principal is
/// `project-admin`. An empty list means no control authority in the org.
pub async fn control_projects(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &wamn_registry::OrgId,
) -> Result<Vec<wamn_registry::ProjectId>, IdentityError> {
    control_projects_page(client, principal_id, org, None, None).await
}

/// Read a keyset page, or all rows when the limit is absent.
pub async fn control_projects_page(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &wamn_registry::OrgId,
    after: Option<&wamn_registry::ProjectId>,
    limit: Option<i64>,
) -> Result<Vec<wamn_registry::ProjectId>, IdentityError> {
    sqlx::query(CONTROL_PROJECTS_SQL)
        .bind(Uuid::from(*principal_id))
        .bind(org.as_str())
        .bind(PROJECT_ADMIN_ROLE)
        .bind(after.map(wamn_registry::ProjectId::as_str))
        .bind(limit)
        .fetch_all(&mut *client)
        .await
        .map_err(|error| database_error(&error))?
        .iter()
        .map(|row| {
            row.try_get::<String, _>(0)
                .and_then(|value| wamn_postgres::decode_text(value, 0))
                .map_err(|error| database_error(&error))
        })
        .collect::<Result<_, _>>()
}

/// Whether the active user principal holds `org-admin` in `org`.
pub async fn is_org_admin(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &wamn_registry::OrgId,
) -> Result<bool, IdentityError> {
    sqlx::query(ORG_ADMIN_SQL)
        .bind(Uuid::from(*principal_id))
        .bind(org.as_str())
        .bind(ORG_ADMIN_ROLE)
        .fetch_one(&mut *client)
        .await
        .map_err(|error| database_error(&error))?
        .try_get(0)
        .map_err(|error| database_error(&error))
}

/// Whether the active user principal holds `project-admin` in `project` of
/// `org`.
pub async fn is_project_admin(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
) -> Result<bool, IdentityError> {
    sqlx::query(PROJECT_ADMIN_SQL)
        .bind(Uuid::from(*principal_id))
        .bind(org.as_str())
        .bind(project.as_str())
        .bind(PROJECT_ADMIN_ROLE)
        .fetch_one(&mut *client)
        .await
        .map_err(|error| database_error(&error))?
        .try_get(0)
        .map_err(|error| database_error(&error))
}

/// A project and its current state.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProjectSummary {
    /// The project name.
    pub name: wamn_registry::ProjectId,
    /// The state exposed by project.list.
    pub state: String,
}

/// Every project of `org`, which an `org-admin` administers.
pub async fn org_projects(
    client: &mut sqlx::PgConnection,
    org: &wamn_registry::OrgId,
) -> Result<Vec<ProjectSummary>, IdentityError> {
    org_projects_page(client, org, None, None).await
}

/// Read a keyset page, or all rows when the limit is absent.
pub async fn org_projects_page(
    client: &mut sqlx::PgConnection,
    org: &wamn_registry::OrgId,
    after: Option<&str>,
    limit: Option<i64>,
) -> Result<Vec<ProjectSummary>, IdentityError> {
    sqlx::query(ORG_PROJECTS_SQL)
        .bind(org.as_str())
        .bind(after)
        .bind(limit)
        .fetch_all(&mut *client)
        .await
        .map_err(|error| database_error(&error))?
        .iter()
        .map(|row| {
            Ok(ProjectSummary {
                name: row
                    .try_get::<String, _>(0)
                    .and_then(|value| wamn_postgres::decode_text(value, 0))
                    .map_err(|error| database_error(&error))?,
                state: row.try_get(1).map_err(|error| database_error(&error))?,
            })
        })
        .collect::<Result<_, IdentityError>>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_org_audience_names_exactly_one_org() {
        assert_eq!(
            org_audience_org("urn:wamn:project-env:dkk:dev:receiving:abc"),
            None
        );
    }
}
