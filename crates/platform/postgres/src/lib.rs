//! Verified PostgreSQL connections from explicit component configuration.

use std::borrow::Cow;

use anyhow::Context as _;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sqlx::postgres::{PgConnectOptions, PgSslMode};

/// A Secret in the organization's namespace.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SecretRef {
    pub namespace: String,
    pub name: String,
}

/// Public connection data. Passwords remain in one basic-auth Secret per role.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionConfig {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    pub credentials: SecretRef,
    pub server_ca: SecretRef,
}

impl ConnectionConfig {
    /// Encode configuration without including password or CA bytes.
    pub fn to_json(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string(self)?)
    }
}

/// Read a mounted component configuration or borrow an explicit command input.
pub fn configuration(input: &str) -> anyhow::Result<Cow<'_, str>> {
    if input.starts_with('/') {
        Ok(Cow::Owned(std::fs::read_to_string(input)?))
    } else {
        Ok(Cow::Borrowed(input))
    }
}

/// Resolve public identity without loading credentials or opening a session.
pub fn identity(input: &str) -> anyhow::Result<(String, String)> {
    let input = configuration(input)?;
    if input.trim_start().starts_with('{') {
        let config: ConnectionConfig = serde_json::from_str(&input)?;
        Ok((config.username, config.database))
    } else {
        let options: PgConnectOptions = input.parse()?;
        Ok((
            options.get_username().into(),
            options.get_database().context("database is absent")?.into(),
        ))
    }
}

fn secret_path(reference: &SecretRef) -> anyhow::Result<String> {
    for value in [&reference.namespace, &reference.name] {
        anyhow::ensure!(
            !value.is_empty()
                && value
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "invalid database Secret reference"
        );
    }
    Ok(format!(
        "/api/v1/namespaces/{}/secrets/{}",
        reference.namespace, reference.name
    ))
}

fn data(secret: &serde_json::Value, key: &str) -> anyhow::Result<Vec<u8>> {
    Ok(STANDARD.decode(
        secret["data"][key]
            .as_str()
            .context("database Secret entry is absent")?,
    )?)
}

/// The one constructor for every pool and dedicated SQL connection.
/// Components read native CNPG Secrets through their in-cluster service account.
/// Operator commands use an explicit PostgreSQL connection string.
pub async fn connect_options<'a>(
    input: impl Into<ConnectionInput<'a>>,
) -> anyhow::Result<PgConnectOptions> {
    let input = match input.into() {
        ConnectionInput::Options(options) => return Ok((*options).ssl_mode(PgSslMode::VerifyFull)),
        ConnectionInput::Configuration(input) => input,
    };
    let input = configuration(input)?;
    let options = if input.trim_start().starts_with('{') {
        let config: ConnectionConfig = serde_json::from_str(&input)?;
        anyhow::ensure!(
            std::env::var_os("KUBERNETES_SERVICE_HOST").is_some(),
            "no credential store in local mode"
        );
        let api = wamn_kubernetes::kubernetes_api::KubernetesApi::in_cluster()?;
        anyhow::ensure!(
            config.credentials.namespace == api.namespace
                && config.server_ca.namespace == api.namespace,
            "database credentials, CA and consumer must share the org namespace"
        );
        let secret = api.get(secret_path(&config.credentials)?).await?;
        anyhow::ensure!(
            secret["type"] == "kubernetes.io/basic-auth",
            "database credential is not a basic-auth Secret"
        );
        let username = String::from_utf8(data(&secret, "username")?)?;
        let password = String::from_utf8(data(&secret, "password")?)?;
        anyhow::ensure!(
            username == config.username,
            "database Secret names another login"
        );
        let ca = api.get(secret_path(&config.server_ca)?).await?;
        PgConnectOptions::new()
            .host(&config.host)
            .port(config.port)
            .database(&config.database)
            .username(&username)
            .password(&password)
            .ssl_root_cert_from_pem(data(&ca, "ca.crt")?)
    } else {
        let explicit = url::Url::parse(&input)?;
        anyhow::ensure!(
            !explicit.username().is_empty() && !explicit.path().trim_matches('/').is_empty(),
            "explicit database login and database are required"
        );
        input.parse::<PgConnectOptions>()?
    };
    Ok(options.ssl_mode(PgSslMode::VerifyFull))
}

/// Open a dedicated session with the same verified options as pooled sessions.
pub async fn connect<'a>(
    input: impl Into<ConnectionInput<'a>>,
) -> anyhow::Result<sqlx::PgConnection> {
    use sqlx::Connection as _;
    Ok(sqlx::PgConnection::connect_with(&connect_options(input).await?).await?)
}

/// Construct one initially idle pool for a role and database.
pub async fn pool(input: &str, max_connections: u32) -> anyhow::Result<sqlx::PgPool> {
    Ok(sqlx::postgres::PgPoolOptions::new()
        .max_connections(max_connections)
        .connect_lazy_with(connect_options(input).await?))
}

/// Explicit connection input for component configuration or database administration.
pub enum ConnectionInput<'a> {
    Configuration(&'a str),
    Options(Box<PgConnectOptions>),
}
impl std::fmt::Debug for ConnectionInput<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionInput").finish_non_exhaustive()
    }
}
impl<'a> From<&'a str> for ConnectionInput<'a> {
    fn from(value: &'a str) -> Self {
        Self::Configuration(value)
    }
}
impl<'a> From<&'a String> for ConnectionInput<'a> {
    fn from(value: &'a String) -> Self {
        Self::Configuration(value)
    }
}
impl From<PgConnectOptions> for ConnectionInput<'_> {
    fn from(value: PgConnectOptions) -> Self {
        Self::Options(Box::new(value))
    }
}

/// Select another database while retaining the explicit login and TLS references.
pub fn database_configuration(input: &str, database: &str) -> anyhow::Result<String> {
    let input = configuration(input)?;
    if input.trim_start().starts_with('{') {
        let mut config: ConnectionConfig = serde_json::from_str(&input)?;
        config.database = database.into();
        config.to_json()
    } else {
        let mut coordinate = url::Url::parse(&input)?;
        coordinate.set_path(database);
        Ok(coordinate.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn explicit_connections_require_identity_and_verified_tls() {
        for input in [
            "postgres://localhost/database",
            "postgres://user@localhost/",
        ] {
            assert!(connect_options(input).await.is_err());
        }
        let options =
            connect_options("postgres://user:password@localhost/database?sslmode=disable")
                .await
                .unwrap();
        assert!(matches!(options.get_ssl_mode(), PgSslMode::VerifyFull));
    }

    #[tokio::test]
    async fn old_url_documents_are_refused() {
        assert!(
            connect_options(r#"{"url":"postgres://user:password@localhost/database"}"#)
                .await
                .is_err()
        );
    }
}

/// An explicit connection names another role or database than its consumer.
#[derive(Debug)]
pub struct IdentityMismatch {
    pub username: String,
    pub database: String,
}
impl std::fmt::Display for IdentityMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("database connection identity refused")
    }
}
impl std::error::Error for IdentityMismatch {}

/// Check component authority before opening a socket or reading credential material.
pub fn require_identity(input: &str, username: &str, database: &str) -> anyhow::Result<()> {
    let (actual_user, actual_database) = identity(input)?;
    if actual_user != username || actual_database != database {
        return Err(IdentityMismatch {
            username: actual_user,
            database: actual_database,
        }
        .into());
    }
    Ok(())
}
