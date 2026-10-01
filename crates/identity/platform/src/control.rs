//! Control authority: who may hold a control session of an org
//! (docs/plan/platform-ui.md §4.3).
//!
//! A control session is org-scoped and carries no application roles. Its
//! audience is derived from the org, so nothing is configured per org. The
//! org role `org-admin`, or the project role `project-admin` in a project of
//! the org, grants control authority in that org.

use tokio_postgres::GenericClient;
use wamn_session::token::{SessionAuthority, SessionClaims};

use crate::org::ORG_ADMIN_ROLE;
use crate::{IdentityError, IdentityErrorType, PrincipalId, database_error};

/// The prefix of every control audience.
pub const CONTROL_AUDIENCE_PREFIX: &str = "urn:wamn:control:";

/// The project role that grants control authority in its org.
pub const PROJECT_ADMIN_ROLE: &str = "project-admin";

const CONTROL_PROJECTS_SQL: &str = "SELECT r.project FROM identity.project_roles r \
    JOIN identity.principals p ON p.id = r.principal_id \
    WHERE r.principal_id = $1::text::uuid AND r.org = $2 AND r.role = $3 \
    AND p.type = 'user' AND p.status = 'active' \
    ORDER BY r.project";

const ORG_PROJECTS_SQL: &str = "SELECT id FROM registry.projects WHERE org = $1 ORDER BY id";

const ORG_ADMIN_SQL: &str = "SELECT EXISTS (SELECT 1 FROM identity.org_roles r \
    JOIN identity.principals p ON p.id = r.principal_id \
    WHERE r.principal_id = $1::text::uuid AND r.org = $2 AND r.role = $3 \
    AND p.type = 'user' AND p.status = 'active')";

const CONTROL_ORGS_SQL: &str = "SELECT o.id FROM registry.orgs o \
    JOIN identity.principals p ON p.id = $1::text::uuid \
    WHERE p.type = 'user' AND p.status = 'active' \
    AND (EXISTS (SELECT 1 FROM identity.project_roles r \
    WHERE r.principal_id = p.id AND r.org = o.id AND r.role = $2) \
    OR EXISTS (SELECT 1 FROM identity.org_roles r \
    WHERE r.principal_id = p.id AND r.org = o.id AND r.role = $3)) \
    ORDER BY o.id";

const CONTROL_SESSION_ACTIVE_SQL: &str = "SELECT EXISTS (SELECT 1 FROM identity.principals p \
    WHERE p.id = $1::text::uuid AND p.type = 'user' AND p.status = 'active' \
    AND (EXISTS (SELECT 1 FROM identity.project_roles r \
    WHERE r.principal_id = p.id AND r.org = $2 AND r.role = $3) \
    OR EXISTS (SELECT 1 FROM identity.org_roles r \
    WHERE r.principal_id = p.id AND r.org = $2 AND r.role = $7)) \
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
    Ok(format!("{CONTROL_AUDIENCE_PREFIX}{org}"))
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
    client: &(impl GenericClient + Sync),
    principal_id: &PrincipalId,
    org: &str,
) -> Result<Vec<String>, IdentityError> {
    if !valid_org(org) {
        return Err(invalid_org());
    }
    Ok(client
        .query(
            CONTROL_PROJECTS_SQL,
            &[&principal_id.as_str(), &org, &PROJECT_ADMIN_ROLE],
        )
        .await
        .map_err(|error| database_error(&error))?
        .iter()
        .map(|row| row.get(0))
        .collect())
}

/// Whether the active user principal holds `org-admin` in `org`.
pub async fn is_org_admin(
    client: &(impl GenericClient + Sync),
    principal_id: &PrincipalId,
    org: &str,
) -> Result<bool, IdentityError> {
    if !valid_org(org) {
        return Err(invalid_org());
    }
    client
        .query_one(
            ORG_ADMIN_SQL,
            &[&principal_id.as_str(), &org, &ORG_ADMIN_ROLE],
        )
        .await
        .map_err(|error| database_error(&error))?
        .try_get(0)
        .map_err(|error| database_error(&error))
}

/// Every project of `org`, which an `org-admin` administers.
pub async fn org_projects(
    client: &(impl GenericClient + Sync),
    org: &str,
) -> Result<Vec<String>, IdentityError> {
    if !valid_org(org) {
        return Err(invalid_org());
    }
    Ok(client
        .query(ORG_PROJECTS_SQL, &[&org])
        .await
        .map_err(|error| database_error(&error))?
        .iter()
        .map(|row| row.get(0))
        .collect())
}

/// Every registered org where the active user principal holds control
/// authority.
pub async fn control_orgs(
    client: &(impl GenericClient + Sync),
    principal_id: &PrincipalId,
) -> Result<Vec<String>, IdentityError> {
    Ok(client
        .query(
            CONTROL_ORGS_SQL,
            &[&principal_id.as_str(), &PROJECT_ADMIN_ROLE, &ORG_ADMIN_ROLE],
        )
        .await
        .map_err(|error| database_error(&error))?
        .iter()
        .map(|row| row.get(0))
        .collect())
}

/// Check, without caching an approval, that a verified control session
/// still has its authority: a live password login for this exact issuer and
/// audience, and a current control role in the token's org.
///
/// Signature and exact audience verification must precede this read. A
/// control session never comes from a PAT.
pub async fn control_session_is_active(
    client: &(impl GenericClient + Sync),
    claims: &SessionClaims,
) -> Result<bool, IdentityError> {
    let SessionAuthority::Login(login) = &claims.authority else {
        return Ok(false);
    };
    if control_audience_org(&claims.aud) != Some(claims.org.as_str()) {
        return Ok(false);
    }
    client
        .query_one(
            CONTROL_SESSION_ACTIVE_SQL,
            &[
                &claims.sub,
                &claims.org,
                &PROJECT_ADMIN_ROLE,
                login,
                &claims.iss,
                &claims.aud,
                &ORG_ADMIN_ROLE,
            ],
        )
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
        assert_eq!(control_audience("dkk").unwrap(), "urn:wamn:control:dkk");
        assert_eq!(control_audience_org("urn:wamn:control:dkk"), Some("dkk"));
        assert_eq!(control_audience_org("urn:wamn:control:"), None);
        assert_eq!(control_audience_org("urn:wamn:control:a:b"), None);
        assert_eq!(
            control_audience_org("urn:wamn:project-env:dkk:receiving:dev:abc"),
            None
        );
    }
}
