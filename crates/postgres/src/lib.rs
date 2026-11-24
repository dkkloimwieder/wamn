//! Verified PostgreSQL connections from explicit component configuration.

use std::path::Path;

use anyhow::Context as _;
use sqlx::postgres::PgConnectOptions;

pub mod build_database;
pub mod catalog;
pub mod control;
pub mod oauth_authorization;
pub mod replication;

pub use build_database::{
    ConnectionConfig, ConnectionInput, DATABASE_DIR, SecretRef, configuration, connect,
    connect_options, credential_directory, database_configuration, identity, pool, read_password,
};

impl ConnectionConfig {
    /// Read public connection configuration from a file.
    pub async fn read(path: &Path) -> anyhow::Result<Self> {
        let contents = tokio::fs::read(path)
            .await
            .with_context(|| format!("read database configuration {}", path.display()))?;
        serde_json::from_slice(&contents)
            .with_context(|| format!("decode database configuration {}", path.display()))
    }
}

/// Read a public connection file and the login directory next to it.
pub async fn mounted_options(path: &Path) -> anyhow::Result<PgConnectOptions> {
    let root = path
        .parent()
        .context("database configuration has no parent directory")?;
    ConnectionConfig::read(path)
        .await?
        .mounted_options(root)
        .await
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
    use sqlx::postgres::PgSslMode;

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

pub mod operation;
pub mod run_store;
pub mod schema;

mod transaction_identity;
pub use transaction_identity::{bind_transaction_identity, transaction_identity_sql};

mod checkout;
mod host_project;
mod package_catalog;
mod pool;

pub use checkout::CheckoutError;
pub use host_project::HostProject;
pub use package_catalog::installed_package_artifacts;
pub use pool::{
    ClassCredentials, CredentialProvider, DEFAULT_PROJECT, Object, ProjectConfig, ProjectPool,
    ResolvedCredential, StaticCredentialProvider, WamnPostgres, WamnPostgresConfig,
    destroy_connection,
};

mod registry;
pub use registry::{
    ENVIRONMENT_LOCK_SQL, ENVIRONMENT_TRANSACTION_LOCK_SQL, LifecycleLock, REGISTRY_HISTORY_TABLE,
    Registry, RegistryTransaction, decode_text,
};
