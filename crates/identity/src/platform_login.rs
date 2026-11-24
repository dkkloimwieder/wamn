//! Project platform login configuration.

use wamn_postgres::database::ConnectionConfig;

/// Select the platform credential from public project connection facts.
pub fn platform_configuration(input: &str) -> anyhow::Result<String> {
    let input = wamn_postgres::configuration(input)?;
    if !input.trim_start().starts_with('{') {
        return Ok(input.into_owned());
    }
    platform_connection(serde_json::from_str(&input)?)?.to_json()
}

/// Select the platform login on the same project endpoint.
pub fn platform_connection(mut config: ConnectionConfig) -> anyhow::Result<ConnectionConfig> {
    config.username = wamn_role::PLATFORM_LOGIN.parse()?;
    config.credentials.name =
        wamn_registry::name::project_login_certificate(&config.database, config.username.as_str());
    Ok(config)
}
