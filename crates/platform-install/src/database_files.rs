//! Stage database credentials and public roots from the worker namespace.

use anyhow::Context as _;
use std::sync::Arc;
use tokio::io::AsyncWriteExt as _;
use wamn_postgres::database::{ConnectionConfig, DATABASE_DIR, credential_directory};

use wamn_kubernetes::client::{KubernetesApi, secret_entry};

/// A worker's Kubernetes access to the database files it stages in its pod.
#[derive(Debug)]
pub struct DatabaseFiles(Arc<KubernetesApi>);

impl Clone for DatabaseFiles {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

enum CaSource {
    ProjectSecret,
    SystemConfigMap,
}

impl DatabaseFiles {
    /// Open the worker's in-cluster API client.
    pub fn in_cluster() -> anyhow::Result<Self> {
        Ok(Self(Arc::new(KubernetesApi::in_cluster()?)))
    }

    /// Stage the project login and CA Secrets in the worker's database volume.
    pub async fn materialize(&self, config: &ConnectionConfig) -> Result<(), DatabaseFilesError> {
        self.materialize_from(config, CaSource::ProjectSecret).await
    }

    /// Stage the system login Secret and public CA ConfigMap in the worker's database volume.
    ///
    /// # Errors
    /// Returns an error if the worker cannot read or stage the files.
    pub async fn materialize_system(
        &self,
        config: &ConnectionConfig,
    ) -> Result<(), DatabaseFilesError> {
        self.materialize_from(config, CaSource::SystemConfigMap)
            .await
    }

    async fn materialize_from(
        &self,
        config: &ConnectionConfig,
        source: CaSource,
    ) -> Result<(), DatabaseFilesError> {
        self.write_files(config, source)
            .await
            .map_err(|source| DatabaseFilesError {
                login: config.credentials.name.clone(),
                ca: config.server_ca.name.clone(),
                source,
            })
    }

    async fn write_files(&self, config: &ConnectionConfig, source: CaSource) -> anyhow::Result<()> {
        use std::path::Path;

        anyhow::ensure!(
            config.credentials.namespace == self.0.namespace
                && config.server_ca.namespace == self.0.namespace,
            "database credential sources and worker must share a namespace"
        );
        let root = Path::new(DATABASE_DIR);
        let directory = credential_directory(root, &config.credentials.name)?;
        credential_directory(root, &config.server_ca.name)?;
        let ca = async {
            match source {
                CaSource::SystemConfigMap => self.0.public_root(&config.server_ca.name).await,
                CaSource::ProjectSecret => {
                    let secret = self.0.secret(&config.server_ca.name).await?;
                    Ok(secret
                        .data
                        .and_then(|mut data| data.remove("ca.crt"))
                        .with_context(|| format!("Secret {} lacks ca.crt", config.server_ca.name))?
                        .0)
                }
            }
        };
        let (login, server_ca) = tokio::try_join!(self.0.secret(&config.credentials.name), ca)?;
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

        let mut builder = tokio::fs::DirBuilder::new();
        builder.mode(0o700);
        match builder.create(&directory).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        for (name, bytes) in credentials
            .into_iter()
            .chain([("ca.crt", server_ca.as_slice())])
        {
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
