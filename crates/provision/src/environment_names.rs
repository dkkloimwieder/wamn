//! Resource names derived from registry coordinates.
use crate::{ProvisionError, validate_project_env};

pub fn environment_resource_name(
    org: &str,
    project: &str,
    env: &wamn_registry::EnvName,
) -> Result<String, ProvisionError> {
    validate_project_env(org, project, env)?;
    Ok(
        wamn_registry::name::environment_resource_name(org, project, env)
            .expect("validated coordinates"),
    )
}
pub fn application_release_name(
    org: &str,
    project: &str,
    env: &wamn_registry::EnvName,
    application: &str,
) -> Result<String, ProvisionError> {
    validate_project_env(org, project, env)?;
    Ok(
        wamn_registry::name::application_release_name(org, project, env, application)
            .expect("validated coordinates"),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn application_names_are_bounded_and_refuse_invalid_coordinates() {
        let env = "a".repeat(30).try_into().unwrap();
        let name = application_release_name("a", "wms", &env, "orders").unwrap();
        assert!(name.len() <= 47);
        assert_ne!(
            name,
            application_release_name("a", "wms", &env, "receiving").unwrap()
        );
    }
}

/// Event coordinates supplied by environment provisioning and its chart consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventIdentity {
    pub org: String,
    pub project: String,
    pub environment: wamn_registry::EnvName,
}
