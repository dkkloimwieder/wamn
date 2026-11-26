//! Project platform login configuration.

use wamn_postgres::database::ConnectionConfig;

/// Select the platform login on the same project endpoint.
pub fn platform_connection(mut config: ConnectionConfig) -> anyhow::Result<ConnectionConfig> {
    config.username = wamn_role::PLATFORM_LOGIN.parse()?;
    config.credentials.name =
        wamn_registry::name::project_login_certificate(&config.database, config.username.as_str());
    Ok(config)
}
