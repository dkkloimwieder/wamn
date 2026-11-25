//! Resource names derived from registry coordinates.
use crate::{error::ProvisionError, name::validate_project_env};

pub fn environment_resource_name(
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
) -> Result<String, ProvisionError> {
    validate_project_env(org, project, env)?;
    Ok(wamn_registry::name::environment_resource_name(
        org, project, env,
    ))
}

/// Event coordinates supplied by environment provisioning and its chart consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventIdentity {
    pub org: wamn_registry::OrgId,
    pub project: wamn_registry::ProjectId,
    pub environment: wamn_registry::EnvName,
}
