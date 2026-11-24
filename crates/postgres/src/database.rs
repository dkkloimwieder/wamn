//! Public database configuration and the credential mount layout.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Root of the database credential mounts in deployed processes.
pub const DATABASE_DIR: &str = "/etc/wamn/db";

/// A named credential source in the organization's namespace.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SecretRef {
    pub namespace: String,
    pub name: String,
}

/// Public connection data. Login and CA bytes come from mounted files.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionConfig {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: wamn_role::LoginName,
    pub credentials: SecretRef,
    pub server_ca: SecretRef,
}

impl ConnectionConfig {
    /// Encode configuration without including credential or CA bytes.
    pub fn to_json(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string(self)?)
    }
}

/// Resolve one named login directory below the supplied mount root.
pub fn credential_directory(root: &Path, name: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(
        !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
        "invalid database credential name"
    );
    Ok(root.join(name))
}

pub fn configuration(
    namespace: &str,
    cluster: &str,
    host: String,
    port: u16,
    database: String,
    username: wamn_role::LoginName,
    secret: String,
) -> ConnectionConfig {
    ConnectionConfig {
        host,
        port,
        database,
        username,
        credentials: SecretRef {
            namespace: namespace.into(),
            name: secret,
        },
        server_ca: SecretRef {
            namespace: namespace.into(),
            name: format!("{cluster}-ca"),
        },
    }
}

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
    configuration(
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
