//! Native CNPG credentials and public connection configuration.

use anyhow::Context as _;
use k8s_openapi::api::core::v1::Secret;
use kube::Api;
use std::{sync::Arc, time::Duration};
use tokio::io::AsyncWriteExt as _;
use wamn_kubernetes::client::KubernetesApi;
use wamn_postgres::{ConnectionConfig, SecretRef};
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

pub async fn wait_role(
    connection: &mut sqlx::PgConnection,
    username: &wamn_role::LoginName,
) -> anyhow::Result<()> {
    wait_role_kind(connection, username, true).await
}

pub(crate) async fn wait_group_role(
    connection: &mut sqlx::PgConnection,
    role: &wamn_role::LoginName,
) -> anyhow::Result<()> {
    wait_role_kind(connection, role, false).await
}

async fn wait_role_kind(
    connection: &mut sqlx::PgConnection,
    role: &wamn_role::LoginName,
    can_login: bool,
) -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let present: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT FROM pg_roles WHERE rolname=$1 AND rolcanlogin=$2)",
            )
            .bind(role.as_str())
            .bind(can_login)
            .fetch_one(&mut *connection)
            .await?;
            if present {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("CNPG did not reconcile database role {role}"))?
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
pub struct DatabaseFiles(Arc<KubernetesApi>);

impl Clone for DatabaseFiles {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl DatabaseFiles {
    /// Open the worker's in-cluster API client.
    pub fn in_cluster() -> anyhow::Result<Self> {
        Ok(Self(Arc::new(KubernetesApi::in_cluster()?)))
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
        let secrets = Api::<Secret>::namespaced(self.0.client(), &self.0.namespace);
        let (login, ca) = tokio::time::timeout(wamn_kubernetes::client::REQUEST_TIMEOUT, async {
            tokio::try_join!(
                secrets.get(&config.credentials.name),
                secrets.get(&config.server_ca.name)
            )
        })
        .await??;
        let credentials = match login.type_.as_deref() {
            Some("kubernetes.io/tls") => [
                ("tls.crt", secret_entry(&login, "tls.crt")?),
                ("tls.key", secret_entry(&login, "tls.key")?),
            ],
            Some("kubernetes.io/basic-auth") => {
                let username = secret_entry(&login, "username")?;
                anyhow::ensure!(
                    username == config.username.as_str().as_bytes(),
                    "database Secret names another login"
                );
                [
                    ("username", username),
                    ("password", secret_entry(&login, "password")?),
                ]
            }
            _ => anyhow::bail!("database credential Secret must be TLS or basic-auth"),
        };
        let server_ca = secret_entry(&ca, "ca.crt")?;

        let mut builder = tokio::fs::DirBuilder::new();
        builder.mode(0o700);
        match builder.create(&directory).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        for (name, bytes) in credentials.into_iter().chain([("ca.crt", server_ca)]) {
            let path = directory.join(name);
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&path)
                .await
                .with_context(|| format!("open {}", path.display()))?;
            file.write_all(bytes)
                .await
                .with_context(|| format!("write {}", path.display()))?;
            file.flush().await?;
        }
        Ok(())
    }
}

fn secret_entry<'a>(secret: &'a Secret, key: &str) -> anyhow::Result<&'a [u8]> {
    secret
        .data
        .as_ref()
        .and_then(|data| data.get(key))
        .map(|bytes| bytes.0.as_slice())
        .with_context(|| format!("database Secret key {key} is absent"))
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
