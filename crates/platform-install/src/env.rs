//! Resource name limits for project environments.

/// A project environment coordinate exceeds a resource name limit.
#[derive(Debug)]
pub struct EnvNameError {
    pub name: String,
    pub max: usize,
}

impl std::fmt::Display for EnvNameError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "provisioned name {:?} is {} bytes, over the {}-byte limit: shorten the org or project id",
            self.name,
            self.name.len(),
            self.max,
        )
    }
}

impl std::error::Error for EnvNameError {}

use wamn_registry::name::{
    MAX_DB_NAME_LEN, MAX_NAMESPACE_STEM_LEN, cdc_object_name, project_env_database_name,
    project_env_namespace_stem,
};

/// Refuse coordinates whose database or namespace name exceeds its limit.
pub fn validate_project_env(
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
) -> Result<(), EnvNameError> {
    let probe = "00000000"
        .parse::<wamn_registry::instance_suffix::InstanceSuffix>()
        .expect("fixed suffix");
    let database = project_env_database_name(org, project, env, &probe);
    if database.len() > MAX_DB_NAME_LEN {
        return Err(EnvNameError {
            name: database,
            max: MAX_DB_NAME_LEN,
        });
    }
    let stem = project_env_namespace_stem(org, project, env);
    if stem.len() > MAX_NAMESPACE_STEM_LEN {
        return Err(EnvNameError {
            name: stem,
            max: MAX_NAMESPACE_STEM_LEN,
        });
    }
    Ok(())
}

/// Refuse coordinates whose CDC object name exceeds the database limit.
pub fn validate_project_env_cdc(
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
) -> Result<(), EnvNameError> {
    validate_project_env(org, project, env)?;
    let probe = "00000000"
        .parse::<wamn_registry::instance_suffix::InstanceSuffix>()
        .expect("fixed suffix");
    let cdc = cdc_object_name(org, project, env, &probe);
    if cdc.len() > MAX_DB_NAME_LEN {
        return Err(EnvNameError {
            name: cdc,
            max: MAX_DB_NAME_LEN,
        });
    }
    Ok(())
}
