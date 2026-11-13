//! Control authority: who may hold a control session of an org
//! (docs/architecture/execution.md#control-sessions).
//!
//! A control session is org-targeted and carries no application roles. Its
//! audience is derived from the org, so nothing is configured per org. The
//! org role `org-admin`, or the project role `project-admin` in a project of
//! the org, grants control authority in that org.

use sqlx::Row as _;
use wamn_identity_protocol::token::{SessionAuthority, SessionClaims};

use crate::org::ORG_ADMIN_ROLE;
use crate::{IdentityError, IdentityErrorType, PrincipalId, database_error};

/// The prefix of every control audience.
pub const CONTROL_AUDIENCE_PREFIX: &str = "urn:wamn:control:";

/// The project role that grants control authority in its org.
pub const PROJECT_ADMIN_ROLE: &str = "project-admin";

const CONTROL_PROJECTS_SQL: &str =
    wamn_registry::sql::IDENTITY_PLATFORM_SRC_CONTROL_CONTROL_PROJECTS_SQL;

const ORG_PROJECTS_SQL: &str = wamn_registry::sql::IDENTITY_PLATFORM_SRC_CONTROL_ORG_PROJECTS_SQL;

const ORG_ADMIN_SQL: &str = "SELECT EXISTS (SELECT 1 FROM identity.org_roles r \
    JOIN identity.principals p ON p.id = r.principal_id \
    WHERE r.principal_id = $1::text::uuid AND r.org = $2 AND r.role = $3 \
    AND p.type = 'user' AND p.status = 'active' \
    AND EXISTS (SELECT 1 FROM identity.org_memberships m WHERE m.principal_id=p.id AND m.org=$2 AND m.status='active'))";

const PROJECT_ADMIN_SQL: &str =
    "SELECT identity.is_project_admin($1::text::uuid,$2,$5,$3) AND $4='project-admin'";

const CONTROL_ORGS_SQL: &str = wamn_registry::sql::IDENTITY_PLATFORM_SRC_CONTROL_CONTROL_ORGS_SQL;

const CONTROL_SESSION_ACTIVE_SQL: &str = "SELECT EXISTS (SELECT 1 FROM identity.principals p \
    WHERE p.id = $1::text::uuid AND p.type = 'user' AND p.status = 'active' \
    AND EXISTS (SELECT 1 FROM identity.org_memberships m WHERE m.principal_id=p.id AND m.org=$2 AND m.status='active') \
    AND (EXISTS (SELECT 1 FROM identity.project_roles r \
    WHERE r.principal_id = p.id AND r.org = $2 AND r.role = $3) \
    OR EXISTS (SELECT 1 FROM identity.org_roles r \
    WHERE r.principal_id = p.id AND r.org = $2 AND r.role = $7) \
    OR EXISTS (SELECT 1 FROM identity.environment_roles r WHERE r.principal_id=p.id AND r.org=$2)) \
    AND EXISTS (SELECT 1 FROM identity.password_logins l WHERE l.id = $4::text::uuid \
    AND l.principal_id = p.id AND l.issuer = $5 AND l.audience = $6 AND l.revoked_at IS NULL \
    AND l.expires_at > clock_timestamp() AND l.renewal_expires_at > clock_timestamp()))";

/// Whether `org` has the spelling of `registry.orgs_id_charset_check`.
fn valid_org(org: &str) -> bool {
    org.len() <= 40
        && org.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9'))
        })
}

fn invalid_org() -> IdentityError {
    IdentityError::new(IdentityErrorType::InvalidInput, "org is not an org id")
}

/// The control audience of an org.
pub fn control_audience(org: &str) -> Result<String, IdentityError> {
    if !valid_org(org) {
        return Err(invalid_org());
    }
    Ok(wamn_registry::name::control_audience(org))
}

/// The org of a control audience, or `None` when the audience is not one.
pub fn control_audience_org(audience: &str) -> Option<&str> {
    audience
        .strip_prefix(CONTROL_AUDIENCE_PREFIX)
        .filter(|org| valid_org(org))
}

/// The projects of `org` where the active user principal is
/// `project-admin`. An empty list means no control authority in the org.
pub async fn control_projects(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &str,
) -> Result<Vec<String>, IdentityError> {
    control_projects_page(client, principal_id, org, None, None).await
}

/// Read a keyset page, or all rows when the limit is absent.
pub async fn control_projects_page(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &str,
    after: Option<&str>,
    limit: Option<i64>,
) -> Result<Vec<String>, IdentityError> {
    if !valid_org(org) {
        return Err(invalid_org());
    }
    Ok(sqlx::query(CONTROL_PROJECTS_SQL)
        .bind(principal_id.to_string().as_str())
        .bind(org)
        .bind(PROJECT_ADMIN_ROLE)
        .bind(after)
        .bind(limit)
        .fetch_all(&mut *client)
        .await
        .map_err(|error| database_error(&error))?
        .iter()
        .map(|row| row.get(0))
        .collect())
}

/// Whether the active user principal holds `org-admin` in `org`.
pub async fn is_org_admin(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: &str,
) -> Result<bool, IdentityError> {
    if !valid_org(org) {
        return Err(invalid_org());
    }
    sqlx::query(ORG_ADMIN_SQL)
        .bind(principal_id.to_string().as_str())
        .bind(org)
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
    org: &str,
    project: &str,
    environment: Option<&str>,
) -> Result<bool, IdentityError> {
    if !valid_org(org) {
        return Err(invalid_org());
    }
    let project = crate::require_target_segment("project", project)?;
    sqlx::query(PROJECT_ADMIN_SQL)
        .bind(principal_id.to_string().as_str())
        .bind(org)
        .bind(&project)
        .bind(PROJECT_ADMIN_ROLE)
        .bind(environment)
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
    pub name: String,
    /// The state exposed by project.list.
    pub state: String,
}

/// Every project of `org`, which an `org-admin` administers.
pub async fn org_projects(
    client: &mut sqlx::PgConnection,
    org: &str,
) -> Result<Vec<ProjectSummary>, IdentityError> {
    org_projects_page(client, org, None, None).await
}

/// Read a keyset page, or all rows when the limit is absent.
pub async fn org_projects_page(
    client: &mut sqlx::PgConnection,
    org: &str,
    after: Option<&str>,
    limit: Option<i64>,
) -> Result<Vec<ProjectSummary>, IdentityError> {
    if !valid_org(org) {
        return Err(invalid_org());
    }
    Ok(sqlx::query(ORG_PROJECTS_SQL)
        .bind(org)
        .bind(after)
        .bind(limit)
        .fetch_all(&mut *client)
        .await
        .map_err(|error| database_error(&error))?
        .iter()
        .map(|row| ProjectSummary {
            name: row.get(0),
            state: row.get(1),
        })
        .collect())
}

/// Every registered org where the active user principal holds control
/// authority.
pub async fn control_orgs(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
) -> Result<Vec<String>, IdentityError> {
    control_orgs_page(client, principal_id, None, None, None).await
}

/// Authorized orgs after a key, restricted by the discovery filter.
pub async fn control_orgs_page(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
    org: Option<&str>,
    after: Option<&str>,
    limit: Option<i64>,
) -> Result<Vec<String>, IdentityError> {
    Ok(sqlx::query(CONTROL_ORGS_SQL)
        .bind(principal_id.to_string().as_str())
        .bind(PROJECT_ADMIN_ROLE)
        .bind(ORG_ADMIN_ROLE)
        .bind(org)
        .bind(after)
        .bind(limit)
        .fetch_all(&mut *client)
        .await
        .map_err(|error| database_error(&error))?
        .iter()
        .map(|row| row.get(0))
        .collect())
}

/// Make sure of, without caching an approval, that a verified control session
/// still has its authority: a live password login for this exact issuer and
/// audience, and a current control role in the token's org.
///
/// Signature and exact audience verification must precede this read. A
/// control session never comes from a PAT.
pub async fn control_session_is_active(
    client: &mut sqlx::PgConnection,
    claims: &SessionClaims,
) -> Result<bool, IdentityError> {
    let SessionAuthority::Login(login) = &claims.authority;
    if control_audience_org(&claims.aud) != Some(claims.org.as_str()) {
        return Ok(false);
    }
    sqlx::query(CONTROL_SESSION_ACTIVE_SQL)
        .bind(claims.sub.to_string())
        .bind(&claims.org)
        .bind(PROJECT_ADMIN_ROLE)
        .bind(login.to_string())
        .bind(&claims.iss)
        .bind(&claims.aud)
        .bind(ORG_ADMIN_ROLE)
        .fetch_one(&mut *client)
        .await
        .map_err(|error| database_error(&error))?
        .try_get(0)
        .map_err(|error| database_error(&error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_control_audience_names_exactly_one_org() {
        assert_eq!(
            control_audience_org("urn:wamn:project-env:dkk:receiving:dev:abc"),
            None
        );
    }
}
