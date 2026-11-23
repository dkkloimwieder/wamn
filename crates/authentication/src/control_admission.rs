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
