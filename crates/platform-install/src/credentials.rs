//! Native CNPG credentials and public connection configuration.

use anyhow::Context as _;
use std::{sync::Arc, time::Duration};
use tokio::io::AsyncWriteExt as _;
use wamn_postgres::{ConnectionConfig, SecretRef};
pub fn configuration(
    namespace: &str,
    cluster: &str,
    host: String,
    port: u16,
    database: String,
    username: wamn_identity_protocol::LoginName,
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

pub async fn wait_role(
    connection: &mut sqlx::PgConnection,
    username: &wamn_identity_protocol::LoginName,
) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    loop {
        let present: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT FROM pg_roles WHERE rolname=$1 AND rolcanlogin)",
        )
        .bind(username)
        .fetch_one(&mut *connection)
        .await?;
        if present {
            return Ok(());
        }
        anyhow::ensure!(
            tokio::time::Instant::now() < deadline,
            "CNPG did not reconcile the database login"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Retry opening a connection while the caller accepts connection failures.
pub(crate) async fn connect_with_retry(
    input: &str,
    mut on_error: impl FnMut(anyhow::Error) -> anyhow::Result<()>,
) -> anyhow::Result<sqlx::PgConnection> {
    loop {
        match wamn_postgres::connect(input).await {
            Ok(connection) => return Ok(connection),
            Err(error) => on_error(error)?,
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// A worker's Kubernetes access to the database files it stages in its pod.
#[derive(Debug)]
pub struct DatabaseFiles(Arc<wamn_kubernetes::kubernetes_api::KubernetesApi>);

impl Clone for DatabaseFiles {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl DatabaseFiles {
    /// Open the worker's in-cluster API client.
    pub fn in_cluster() -> anyhow::Result<Self> {
        Ok(Self(Arc::new(
            wamn_kubernetes::kubernetes_api::KubernetesApi::in_cluster()?,
        )))
    }

    /// Stage the named login and CA in the worker's database volume.
    pub async fn materialize(&self, config: &ConnectionConfig) -> Result<(), DatabaseFilesError> {
        self.write_files(config)
            .await
            .map_err(|source| DatabaseFilesError {
                login: config.credentials.name.clone(),
                ca: config.server_ca.name.clone(),
                source,
            })
    }

    async fn write_files(&self, config: &ConnectionConfig) -> anyhow::Result<()> {
        use std::path::Path;

        anyhow::ensure!(
            config.credentials.namespace == self.0.namespace
                && config.server_ca.namespace == self.0.namespace,
            "database credential sources and worker must share the org namespace"
        );
        let root = Path::new(wamn_postgres::DATABASE_DIR);
        let directory = wamn_postgres::credential_directory(root, &config.credentials.name)?;
        wamn_postgres::credential_directory(root, &config.server_ca.name)?;
        let read = |name| {
            self.0.get(format!(
                "/api/v1/namespaces/{}/secrets/{name}",
                self.0.namespace
            ))
        };
        let (login, ca) =
            tokio::try_join!(read(&config.credentials.name), read(&config.server_ca.name),)?;
        anyhow::ensure!(
            login["type"] == "kubernetes.io/basic-auth",
            "database credential is not a basic-auth Secret"
        );
        let username = secret_entry(&login, "username")?;
        anyhow::ensure!(
            username == config.username.as_str().as_bytes(),
            "database Secret names another login"
        );
        let password = secret_entry(&login, "password")?;
        let certificate = secret_entry(&ca, "ca.crt")?;

        let mut builder = tokio::fs::DirBuilder::new();
        builder.mode(0o700);
        match builder.create(&directory).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        for (name, bytes) in [
            ("username", username),
            ("password", password),
            ("ca.crt", certificate),
        ] {
            let path = directory.join(name);
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&path)
                .await
                .with_context(|| format!("open {}", path.display()))?;
            file.write_all(&bytes)
                .await
                .with_context(|| format!("write {}", path.display()))?;
            file.flush().await?;
        }
        Ok(())
    }
}

fn secret_entry(secret: &serde_json::Value, key: &str) -> anyhow::Result<Vec<u8>> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let value = secret["data"][key]
        .as_str()
        .with_context(|| format!("database Secret key {key} is absent"))?;
    Ok(STANDARD
        .decode(value)
        .with_context(|| format!("decode database Secret key {key}"))?)
}

/// The worker could not stage a database's login or CA files.
#[derive(Debug)]
pub struct DatabaseFilesError {
    login: String,
    ca: String,
    source: anyhow::Error,
}

impl std::fmt::Display for DatabaseFilesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "stage database files for login {} and CA {}",
            self.login, self.ca
        )
    }
}

impl std::error::Error for DatabaseFilesError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}
