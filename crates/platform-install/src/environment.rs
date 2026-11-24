//! Connection settings for an environment database.

use crate::database::{self, ConnectionConfig};

/// Resolve a project endpoint from a generation already read from the registry.
pub fn connection_for(
    triple: &wamn_registry::Triple,
    instance: &wamn_registry::instance_suffix::InstanceSuffix,
) -> ConnectionConfig {
    use wamn_registry::name;
    let database =
        name::project_env_database_name(&triple.org, &triple.project, &triple.env, instance);
    let owner = project_migration_login();
    let certificate = name::project_login_certificate(&database, owner.as_str());
    database::configuration(
        triple.org.as_str(),
        &name::project_cluster(&database),
        name::project_database_host(&database, &triple.org),
        5432,
        database,
        owner,
        certificate,
    )
}

/// Derive the project capture credential from the same endpoint as its owner.
pub fn capture_configuration(mut config: ConnectionConfig) -> anyhow::Result<ConnectionConfig> {
    config.username = wamn_role::CDC_LOGIN.parse()?;
    config.credentials.name =
        wamn_registry::name::project_login_certificate(&config.database, config.username.as_str());
    Ok(config)
}

/// The fixed owner login in each project database.
pub fn project_migration_login() -> wamn_role::LoginName {
    wamn_role::MIGRATION_ROLE
        .parse()
        .expect("fixed project owner")
}

/// A project environment coordinate exceeds a resource name limit.
#[derive(Debug)]
pub struct EnvironmentNameError {
    pub name: String,
    pub max: usize,
}

impl std::fmt::Display for EnvironmentNameError {
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

impl std::error::Error for EnvironmentNameError {}

use wamn_registry::name::{
    MAX_DB_NAME_LEN, MAX_NAMESPACE_STEM_LEN, cdc_object_name, project_env_database_name,
    project_env_namespace_stem,
};

/// Refuse coordinates whose database or namespace name exceeds its limit.
pub fn validate_project_env(
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
) -> Result<(), EnvironmentNameError> {
    let probe = "00000000"
        .parse::<wamn_registry::instance_suffix::InstanceSuffix>()
        .expect("fixed suffix");
    let database = project_env_database_name(org, project, env, &probe);
    if database.len() > MAX_DB_NAME_LEN {
        return Err(EnvironmentNameError {
            name: database,
            max: MAX_DB_NAME_LEN,
        });
    }
    let stem = project_env_namespace_stem(org, project, env);
    if stem.len() > MAX_NAMESPACE_STEM_LEN {
        return Err(EnvironmentNameError {
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
) -> Result<(), EnvironmentNameError> {
    validate_project_env(org, project, env)?;
    let probe = "00000000"
        .parse::<wamn_registry::instance_suffix::InstanceSuffix>()
        .expect("fixed suffix");
    let cdc = cdc_object_name(org, project, env, &probe);
    if cdc.len() > MAX_DB_NAME_LEN {
        return Err(EnvironmentNameError {
            name: cdc,
            max: MAX_DB_NAME_LEN,
        });
    }
    Ok(())
}
