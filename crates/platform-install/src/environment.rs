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
