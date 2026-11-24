//! Admission rules for control operations on an organization and its projects.

use wamn_registry::ProjectId;

/// Admission after checking the caller's organization role.
#[derive(Debug)]
pub enum AdminAdmission<'a> {
    Admitted,
    CheckProject(&'a ProjectId),
    Refused,
}

/// Admit an organization administrator or require the selected project's administrator role.
pub fn admin_admission(org_admin: bool, project: Option<&ProjectId>) -> AdminAdmission<'_> {
    if org_admin {
        return AdminAdmission::Admitted;
    }
    match project {
        Some(project) => AdminAdmission::CheckProject(project),
        None => AdminAdmission::Refused,
    }
}

/// Admit an organization administrator or an administrator of any of its projects.
///
/// # Errors
/// Returns an error if a required role cannot be checked.
pub async fn admits_member<E>(
    org_admin: bool,
    has_admin_project: impl Future<Output = Result<bool, E>>,
) -> Result<bool, E> {
    Ok(org_admin || has_admin_project.await?)
}

/// The org role that holds every project and environment of its org.
pub const ORG_ADMIN_ROLE: &str = "org-admin";

/// The project role that grants control authority in its org.
pub const PROJECT_ADMIN_ROLE: &str = "project-admin";

/// The org of an org audience, or `None` when the audience is not one.
pub fn org_audience_org(audience: &str) -> Option<wamn_registry::OrgId> {
    audience
        .strip_prefix(wamn_registry::name::ORG_AUDIENCE_PREFIX)?
        .parse()
        .ok()
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
