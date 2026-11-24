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
pub fn application_release_name(
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
    application: &wamn_registry::ApplicationName,
) -> Result<String, ProvisionError> {
    validate_project_env(org, project, env)?;
    Ok(wamn_registry::resource_names::application_release_name(
        org,
        project,
        env,
        application,
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn application_names_are_bounded_and_refuse_invalid_coordinates() {
        let env = "a".repeat(30).try_into().unwrap();
        let name = application_release_name(
            &"a".parse().unwrap(),
            &"wms".parse().unwrap(),
            &env,
            &"orders".parse().unwrap(),
        )
        .unwrap();
        assert!(name.len() <= 47);
        assert_ne!(
            name,
            application_release_name(
                &"a".parse().unwrap(),
                &"wms".parse().unwrap(),
                &env,
                &"receiving".parse().unwrap()
            )
            .unwrap()
        );
    }
}

/// Event coordinates supplied by environment provisioning and its chart consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventIdentity {
    pub org: wamn_registry::OrgId,
    pub project: wamn_registry::ProjectId,
    pub environment: wamn_registry::EnvName,
}
