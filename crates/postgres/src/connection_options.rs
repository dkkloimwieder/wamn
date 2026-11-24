//! Verified PostgreSQL connection options and mounted credentials.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use sqlx::{
    Connection as _,
    postgres::{PgConnectOptions, PgSslMode},
};

use crate::database::{ConnectionConfig, DATABASE_DIR, credential_directory};

/// Open options using the named credential directory below the supplied mount root.
async fn credential_options(
    config: &ConnectionConfig,
    root: &Path,
) -> anyhow::Result<PgConnectOptions> {
    let directory = credential_directory(root, &config.credentials.name)?;
    let options = PgConnectOptions::new_without_pgpass()
        .host(&config.host)
        .port(config.port)
        .database(&config.database)
        .username(config.username.as_str())
        .ssl_root_cert(directory.join("ca.crt"))
        .ssl_mode(PgSslMode::VerifyFull);

    let cert = directory.join("tls.crt");
    let key = directory.join("tls.key");
    let has_cert = credential_exists(&cert).await?;
    let has_key = credential_exists(&key).await?;
    if !has_cert && !has_key {
        let password = read_password(&directory, &config.username).await?;
        return Ok(options.password(&password));
    }

    if !has_cert || !has_key || credential_exists(&directory.join("password")).await? {
        return Err(CertificateLayoutError { directory }.into());
    }
    // SQLx reads the files on each new connection, including after certificate renewal.
    Ok(options.ssl_client_cert(cert).ssl_client_key(key))
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
pub fn identity(input: &str) -> anyhow::Result<(wamn_role::LoginName, String)> {
    let input = configuration(input)?;
    if input.trim_start().starts_with('{') {
        let config: ConnectionConfig = serde_json::from_str(&input)?;
        Ok((config.username, config.database))
    } else {
        let options: PgConnectOptions = input.parse()?;
        Ok((
            options.get_username().parse()?,
            options.get_database().context("database is absent")?.into(),
        ))
    }
}

/// Read a mounted password after checking the login name.
pub async fn read_password(
    directory: &Path,
    expected: &wamn_role::LoginName,
) -> anyhow::Result<String> {
    let username: wamn_role::LoginName = read_text(&directory.join("username")).await?.parse()?;
    if &username != expected {
        return Err(CredentialMismatch {
            directory: directory.to_owned(),
        }
        .into());
    }
    Ok(read_text(&directory.join("password")).await?)
}

async fn credential_exists(path: &Path) -> Result<bool, CredentialReadError> {
    tokio::fs::try_exists(path)
        .await
        .map_err(|source| CredentialReadError {
            path: path.to_owned(),
            source,
        })
}

async fn read_text(path: &Path) -> Result<String, CredentialReadError> {
    tokio::fs::read_to_string(path)
        .await
        .map_err(|source| CredentialReadError {
            path: path.to_owned(),
            source,
        })
}

#[derive(Debug)]
struct CredentialReadError {
    path: PathBuf,
    source: std::io::Error,
}

impl std::fmt::Display for CredentialReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "read database credential {}", self.path.display())
    }
}

impl std::error::Error for CredentialReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

#[derive(Debug)]
struct CredentialMismatch {
    directory: PathBuf,
}

impl std::fmt::Display for CredentialMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "database credential {} names another login",
            self.directory.display()
        )
    }
}

impl std::error::Error for CredentialMismatch {}

#[derive(Debug)]
struct CertificateLayoutError {
    directory: PathBuf,
}

impl std::fmt::Display for CertificateLayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "database credential {} requires tls.crt and tls.key together without a password file",
            self.directory.display()
        )
    }
}

impl std::error::Error for CertificateLayoutError {}

/// Resolve verified options from mounted files or explicit operator input.
pub async fn connect_options<'a>(
    input: impl Into<ConnectionInput<'a>>,
) -> anyhow::Result<PgConnectOptions> {
    let input = match input.into() {
        ConnectionInput::Options(options) => {
            let username: wamn_role::LoginName = options.get_username().parse()?;
            return Ok((*options)
                .username(username.as_str())
                .ssl_mode(PgSslMode::VerifyFull));
        }
        ConnectionInput::Configuration(input) => input,
    };
    let input = configuration(input)?;
    let options = if input.trim_start().starts_with('{') {
        let config: ConnectionConfig = serde_json::from_str(&input)?;
        credential_options(&config, Path::new(DATABASE_DIR)).await?
    } else {
        let explicit = url::Url::parse(&input)?;
        // SQLx supplies a default login when the URL omits it.
        anyhow::ensure!(
            !explicit.username().is_empty() && !explicit.path().trim_matches('/').is_empty(),
            "explicit database login and database are required"
        );
        let options: PgConnectOptions = input.parse()?;
        let username: wamn_role::LoginName = options.get_username().parse()?;
        options.username(username.as_str())
    };
    Ok(options.ssl_mode(PgSslMode::VerifyFull))
}

/// Open a dedicated session with the same verified options as pooled sessions.
pub async fn connect<'a>(
    input: impl Into<ConnectionInput<'a>>,
) -> anyhow::Result<sqlx::PgConnection> {
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
impl<'a> From<&'a str> for ConnectionInput<'a> {
    fn from(value: &'a str) -> Self {
        Self::Configuration(value)
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

/// Read public connection configuration from a file.
pub async fn read_connection_config(path: &Path) -> anyhow::Result<ConnectionConfig> {
    let contents = tokio::fs::read(path)
        .await
        .with_context(|| format!("read database configuration {}", path.display()))?;
    serde_json::from_slice(&contents)
        .with_context(|| format!("decode database configuration {}", path.display()))
}

/// Read a public connection file and the login directory next to it.
pub async fn mounted_options(path: &Path) -> anyhow::Result<PgConnectOptions> {
    let root = path
        .parent()
        .context("database configuration has no parent directory")?;
    let config = read_connection_config(path).await?;
    credential_options(&config, root).await
}

impl std::fmt::Debug for ConnectionInput<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionInput").finish_non_exhaustive()
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
    pub username: wamn_role::LoginName,
    pub database: String,
}
impl std::fmt::Display for IdentityMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("database connection identity refused")
    }
}
impl std::error::Error for IdentityMismatch {}

/// Check component authority before opening a socket or reading credential material.
pub fn require_identity(
    input: &str,
    username: &wamn_role::LoginName,
    database: &str,
) -> anyhow::Result<()> {
    let (actual_user, actual_database) = identity(input)?;
    if &actual_user != username || actual_database != database {
        return Err(IdentityMismatch {
            username: actual_user,
            database: actual_database,
        }
        .into());
    }
    Ok(())
}
