//! Connection rows and credential references read only through the project platform login.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use moka::sync::Cache;
use tokio::sync::OnceCell;
use uuid::Uuid;
use wamn_kubernetes::{
    ErrorType,
    kubernetes::{CredentialSecret, CredentialSecrets, KubernetesCredentials},
};
use wamn_registry::ProjectEnvKey;

use crate::connection_memory::bounded;

/// Save and retire credential values without exposing the storage client.
#[async_trait::async_trait]
pub trait WriteCredentials: Send + Sync {
    /// Save a new immutable version under the project's Secret name.
    async fn put(
        &self,
        target: CredentialSecret<'_>,
        id: Uuid,
        value: &[u8],
    ) -> anyhow::Result<StoreVersion>;

    /// Remove an immutable version, including when it is already absent.
    async fn destroy(&self, reference: &str, version: &str) -> anyhow::Result<()>;
}

/// Read, save, and retire control-plane credential versions.
#[derive(Debug, Default)]
pub struct KubernetesStore<'a> {
    namespace: Option<&'a str>,
}

impl<'a> KubernetesStore<'a> {
    /// Use a namespace already resolved from the project environment.
    pub fn in_namespace(namespace: &'a str) -> Self {
        Self {
            namespace: Some(namespace),
        }
    }

    fn credentials(&self) -> Result<CredentialSecrets, wamn_kubernetes::Error> {
        match self.namespace {
            Some(namespace) => CredentialSecrets::in_namespace(namespace),
            None => CredentialSecrets::in_cluster(),
        }
    }

    pub async fn read(&self, reference: &str, version: &str) -> anyhow::Result<Vec<u8>> {
        let store = self.credentials().map_err(|error| {
            let local = error.type_ == Some(ErrorType::NoCluster);
            let error = anyhow::Error::new(error);
            if local {
                let reason = "no credential store in local mode";
                tracing::warn!("{reason}");
                error.context(reason)
            } else {
                error
            }
        })?;
        Ok(store.get(reference, version).await?)
    }
}

#[async_trait::async_trait]
impl WriteCredentials for KubernetesStore<'_> {
    async fn put(
        &self,
        target: CredentialSecret<'_>,
        id: Uuid,
        value: &[u8],
    ) -> anyhow::Result<StoreVersion> {
        let version = self.credentials()?.put(&target, id, value).await?;
        Ok(StoreVersion {
            store: "k8s",
            reference: target.name,
            version,
        })
    }

    async fn destroy(&self, reference: &str, version: &str) -> anyhow::Result<()> {
        Ok(self.credentials()?.destroy(reference, version).await?)
    }
}

/// Destroy the stored credential versions after the inactive state commits.
pub async fn destroy_credentials(
    store: &dyn WriteCredentials,
    credentials: &serde_json::Value,
) -> Result<(), crate::administration::ConnectionError> {
    for credential in credentials.as_array().into_iter().flatten() {
        store
            .destroy(
                credential["ref"].as_str().expect("database reference"),
                credential["version"].as_str().expect("database version"),
            )
            .await
            .map_err(crate::administration::store_error)?;
    }
    Ok(())
}

/// The immutable database reference returned by a credential write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreVersion {
    pub store: &'static str,
    pub reference: String,
    pub version: String,
}

/// Credential material omitted from debug output.
pub struct Secret(String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret(..)")
    }
}

type Key = (ProjectEnvKey, Uuid);
type Read = OnceCell<Result<Arc<Secret>, Arc<ReadError>>>;

#[derive(Debug)]
struct ReadError {
    key: Key,
    source: anyhow::Error,
}

impl fmt::Display for ReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "read credential {} for {}: {}",
            self.key.1, self.key.0, self.source
        )
    }
}

impl std::error::Error for ReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// The host's shared Secret reader and bounded cache of credential values.
pub struct CredentialReader {
    store: KubernetesCredentials,
    values: Cache<Key, Arc<Secret>>,
    pending: Mutex<HashMap<Key, Arc<Read>>>,
}

impl fmt::Debug for CredentialReader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialReader")
            .finish_non_exhaustive()
    }
}

impl CredentialReader {
    pub fn new(store: KubernetesCredentials) -> Self {
        Self {
            store,
            values: bounded(),
            pending: Mutex::default(),
        }
    }

    async fn read(
        &self,
        key: Key,
        rejected: Option<&Arc<Secret>>,
        load: impl Future<Output = anyhow::Result<String>>,
    ) -> anyhow::Result<Arc<Secret>> {
        if rejected.is_none()
            && let Some(value) = self.values.get(&key)
        {
            return Ok(value);
        }
        let read = {
            let mut pending = self.pending.lock().expect("credential read lock poisoned");
            if let Some(value) = self.values.get(&key) {
                if rejected.is_none_or(|old| !Arc::ptr_eq(old, &value)) {
                    return Ok(value);
                }
                self.values.invalidate(&key);
            }
            Arc::clone(pending.entry(key.clone()).or_default())
        };
        let pending = PendingRead {
            reader: self,
            key,
            read,
        };
        pending
            .read
            .get_or_init(|| async {
                load.await
                    .map(|value| Arc::new(Secret(value)))
                    .map_err(|source| {
                        Arc::new(ReadError {
                            key: pending.key.clone(),
                            source,
                        })
                    })
            })
            .await
            .as_ref()
            .map(Arc::clone)
            .map_err(|error| anyhow::Error::new(Arc::clone(error)))
    }
}

struct PendingRead<'a> {
    reader: &'a CredentialReader,
    key: Key,
    read: Arc<Read>,
}

impl Drop for PendingRead<'_> {
    fn drop(&mut self) {
        let mut pending = self
            .reader
            .pending
            .lock()
            .expect("credential read lock poisoned");
        if !pending
            .get(&self.key)
            .is_some_and(|read| Arc::ptr_eq(read, &self.read))
        {
            return;
        }
        // Publish and detach together so a rejection of the new value starts a new read.
        match self.read.get() {
            Some(Ok(value)) => {
                self.reader
                    .values
                    .insert(self.key.clone(), Arc::clone(value));
            }
            Some(Err(_)) => {}
            // The map and this guard hold the last two references after cancellation.
            None if Arc::strong_count(&self.read) == 2 => {}
            None => return,
        }
        pending.remove(&self.key);
    }
}

#[derive(Clone)]
struct ProjectReader {
    database: std::sync::Weak<wamn_postgres::WamnPostgres>,
    project: wamn_registry::ProjectId,
    env: wamn_registry::EnvName,
}

/// Connection credentials selected through the project platform pool.
pub struct ConnectionCredentials {
    org: Option<wamn_registry::OrgId>,
    secrets: Option<Arc<CredentialReader>>,
    project: Option<ProjectReader>,
}

impl Clone for ConnectionCredentials {
    fn clone(&self) -> Self {
        Self {
            org: self.org.clone(),
            secrets: self.secrets.as_ref().map(Arc::clone),
            project: self.project.clone(),
        }
    }
}

impl std::fmt::Debug for ConnectionCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConnectionCredentials")
            .field("available", &self.project.is_some())
            .finish_non_exhaustive()
    }
}

impl ConnectionCredentials {
    /// Read connection rows without a system database.
    pub fn for_org(org: &wamn_registry::OrgId, secrets: Option<Arc<CredentialReader>>) -> Self {
        Self {
            org: Some(org.to_owned()),
            secrets,
            project: None,
        }
    }

    pub(crate) fn with_project_database(
        mut self,
        database: std::sync::Weak<wamn_postgres::WamnPostgres>,
        project: &wamn_registry::ProjectId,
        env: &wamn_registry::EnvName,
    ) -> Self {
        self.project = Some(ProjectReader {
            database,
            project: project.to_owned(),
            env: env.to_owned(),
        });
        self
    }

    pub(crate) fn project_connections(
        &self,
        project: &wamn_registry::ProjectId,
        env: &wamn_registry::EnvName,
    ) -> anyhow::Result<Arc<wamn_postgres::WamnPostgres>> {
        let target = self
            .project
            .as_ref()
            .context("project database unavailable")?;
        anyhow::ensure!(
            &target.project == project && &target.env == env,
            "connection project mismatch"
        );
        target
            .database
            .upgrade()
            .context("project database was unloaded")
    }

    pub fn org(&self) -> anyhow::Result<&wamn_registry::OrgId> {
        self.org.as_ref().context("connection reader has no org")
    }

    /// Keep the explicit unavailable state for hosts without a credential reader.
    pub fn empty() -> Self {
        Self {
            org: None,
            secrets: None,
            project: None,
        }
    }

    /// Build a pinned handle from this reader's organization.
    pub fn connection_handle(&self, name: &str, generation: i64) -> anyhow::Result<String> {
        let org = self
            .org
            .as_ref()
            .context("connection credentials unavailable")?;
        let handle = format!("org:{org}:{name}@{generation}");
        self.connection_name(&handle)?;
        Ok(handle)
    }

    pub fn connection_name<'a>(&self, handle: &'a str) -> anyhow::Result<&'a str> {
        let (org, name) = wamn_connection_type::parse_org_credential_handle(handle)
            .map_err(anyhow::Error::msg)?;
        let configured_org = self
            .org
            .as_ref()
            .context("connection credentials unavailable")?;
        anyhow::ensure!(configured_org == &org, "credential org mismatch");
        Ok(name)
    }
}

/// The connection memory reads (docs/architecture/execution.md#connection-memory).
/// Every read but [`ConnectionCredentials::connection_fact`] and
/// [`ConnectionCredentials::env_connections`] returns rows that never change
/// under their keys.
impl ConnectionCredentials {
    /// The active generation, status, credential, principal and lease coordinates of one
    /// connection of the environment.
    pub async fn connection_fact(
        &self,
        project: &wamn_registry::ProjectId,
        env: &wamn_registry::EnvName,
        id: &uuid::Uuid,
    ) -> anyhow::Result<Option<crate::connection_memory::ConnectionFact>> {
        let database = self.project_connections(project, env)?;
        let row = crate::project_connections::connection_fact(&database, project, id).await?;
        Ok(row.map(|row| crate::connection_memory::ConnectionFact {
            active_generation: row.active_generation,
            status: if row.active {
                crate::connection_memory::ConnectionStatus::Active
            } else {
                crate::connection_memory::ConnectionStatus::Disabled
            },
            credential_id: row.credential_id,
            target_id: row.target_id,
            resource_key: row.resource_key,
            authority: row.authority,
            service_principal_id: row.service_principal_id,
        }))
    }

    /// One generation of one connection of the environment.
    pub async fn connection_generation(
        &self,
        project: &wamn_registry::ProjectId,
        env: &wamn_registry::EnvName,
        id: &uuid::Uuid,
        generation: i64,
    ) -> anyhow::Result<Option<ConnectionGeneration>> {
        let database = self.project_connections(project, env)?;
        crate::project_connections::connection_generation(&database, project, id, generation).await
    }

    /// Read the named project Secret only on a cache miss or after rejection.
    pub(crate) async fn credential_secret(
        &self,
        key: &ProjectEnvKey,
        project: &wamn_registry::ProjectId,
        env: &wamn_registry::EnvName,
        credential: Uuid,
        rejected: Option<&Arc<Secret>>,
    ) -> anyhow::Result<Arc<Secret>> {
        let reader = self
            .secrets
            .as_ref()
            .context("credential reader unavailable")?;
        reader
            .read((key.to_owned(), credential), rejected, async {
                let database = self.project_connections(project, env)?;
                let reference = crate::project_connections::credential_reference(
                    &database,
                    project,
                    &credential,
                )
                .await?;
                let reference = reference.context("credential unavailable")?;
                let name = wamn_registry::credential_secret_name(self.org()?, project, env);
                anyhow::ensure!(
                    reference.reference == name,
                    "credential Secret does not match project"
                );
                let bytes = Box::pin(reader.store.read(&name, &reference.version)).await?;
                String::from_utf8(bytes).context("credential is not UTF-8")
            })
            .await
    }

    /// The map from integration name to connection id of the environment.
    pub async fn env_connections(
        &self,
        project: &wamn_registry::ProjectId,
        env: &wamn_registry::EnvName,
    ) -> anyhow::Result<std::collections::BTreeMap<String, uuid::Uuid>> {
        let database = self.project_connections(project, env)?;
        crate::project_connections::env_connections(&database, project).await
    }
}

pub use wamn_registry::connections::ConnectionGeneration;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_absent_reader_is_unavailable() {
        assert!(
            ConnectionCredentials::empty()
                .connection_fact(
                    &"project-a".parse().unwrap(),
                    &"dev".parse().unwrap(),
                    &uuid::Uuid::new_v4()
                )
                .await
                .is_err()
        );
    }
}
