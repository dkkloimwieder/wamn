//! The connection facts of a call, from the connection memory and from
//! Postgres (docs/architecture/execution.md#connection-memory).
//!

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use anyhow::Context as _;
use moka::policy::EvictionPolicy;
use moka::sync::Cache;

use crate::connection_credentials::{
    ConnectionCredentials, ConnectionGeneration, IntegrationVersion,
};
use crate::connection_memory::{ConnectionFact, ConnectionMemory, EnvConnections};
use crate::database::ProjectDatabase;

/// The number of entries of each bounded map.
pub const MAP_ENTRIES: u64 = 1024;

/// A bounded map that evicts its least recently used entry.
pub fn bounded<K, V>() -> Cache<K, V>
where
    K: std::hash::Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    Cache::builder()
        .max_capacity(MAP_ENTRIES)
        .eviction_policy(EvictionPolicy::lru())
        .build()
}

/// The secret of one credential. Debug output omits it.
pub struct Secret(String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Secret(..)")
    }
}

/// The connection facts of one project deployment.
pub struct ConnectionFacts {
    oauth_encryption: Option<Arc<crate::oauth_encryption::Envelope>>,
    memory: Arc<ConnectionMemory>,
    reader: Arc<ConnectionCredentials>,
    project_env_key: wamn_registry::ProjectEnvKey,
    project: wamn_registry::ProjectId,
    environment: wamn_registry::EnvName,
    generations: Cache<(uuid::Uuid, i64), Arc<ConnectionGeneration>>,
    versions:
        Cache<(wamn_registry::integration_code::IntegrationCode, i32), Arc<IntegrationVersion>>,
    secrets: Cache<uuid::Uuid, Arc<Secret>>,
    /// The last fact this host saw of each connection.
    seen: Mutex<HashMap<uuid::Uuid, ConnectionFact>>,
}

impl std::fmt::Debug for ConnectionFacts {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConnectionFacts")
            .field("project_env_key", &self.project_env_key)
            .finish_non_exhaustive()
    }
}

impl ConnectionFacts {
    /// Read one project without retaining its database plugin.
    /// The host keeps PostgreSQL alive while it serves the project.
    pub fn new(
        memory: Arc<ConnectionMemory>,
        reader: Arc<ConnectionCredentials>,
        postgres: &Arc<impl ProjectDatabase + 'static>,
        project_env_key: &wamn_registry::ProjectEnvKey,
        project: &wamn_registry::ProjectId,
        environment: &wamn_registry::EnvName,
    ) -> Self {
        let database = Arc::downgrade(postgres);
        let database: std::sync::Weak<dyn ProjectDatabase> = database;
        Self {
            oauth_encryption: None,
            memory,
            reader: Arc::new(Arc::unwrap_or_clone(reader).with_project_database(
                std::sync::Weak::clone(&database),
                project,
                environment,
            )),
            project_env_key: project_env_key.to_owned(),
            project: project.to_owned(),
            environment: environment.to_owned(),
            generations: bounded(),
            versions: bounded(),
            secrets: bounded(),
            seen: Mutex::new(HashMap::new()),
        }
    }

    /// Share this host's envelope key cache with its OAuth operations.
    pub fn with_oauth_encryption(
        mut self,
        envelope: Option<Arc<crate::oauth_encryption::Envelope>>,
    ) -> Self {
        self.oauth_encryption = envelope;
        self
    }

    pub fn oauth_encryption(&self) -> Option<Arc<crate::oauth_encryption::Envelope>> {
        self.oauth_encryption.as_ref().map(Arc::clone)
    }

    pub fn oauth_tokens(&self) -> Arc<crate::oauth_token::Cache> {
        Arc::clone(&self.memory.oauth_tokens)
    }

    pub fn environment(&self) -> &wamn_registry::EnvName {
        &self.environment
    }

    pub fn project(&self) -> &wamn_registry::ProjectId {
        &self.project
    }

    pub fn project_env_key(&self) -> &wamn_registry::ProjectEnvKey {
        &self.project_env_key
    }

    /// The credential reader used by connection authentication.
    pub fn reader(&self) -> Arc<ConnectionCredentials> {
        Arc::clone(&self.reader)
    }

    /// The credential handle of generation `generation` of connection `name`.
    pub fn credential_handle(&self, name: &str, generation: i64) -> anyhow::Result<String> {
        self.reader.connection_handle(name, generation)
    }

    /// The fact of connection `id`, or `None` when the environment has no
    /// such connection.
    pub async fn connection(&self, id: &uuid::Uuid) -> anyhow::Result<Option<ConnectionFact>> {
        let fact = self
            .reader
            .connection_fact(&self.project, &self.environment, id)
            .await?;
        self.observe(id, fact.as_ref());
        Ok(fact)
    }

    /// Drop the secret and read the fact again from Postgres. A sender calls this
    /// once after the external system refuses the credential.
    pub async fn read_again(&self, id: &uuid::Uuid) -> anyhow::Result<Option<ConnectionFact>> {
        let old = self
            .seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
        if let Some(credential) = old.and_then(|fact| fact.credential_id) {
            self.secrets.invalidate(&credential);
        }
        let fact = self
            .reader
            .connection_fact(&self.project, &self.environment, id)
            .await?;
        self.observe(id, fact.as_ref());
        Ok(fact)
    }

    /// Drop the secret that the new fact of `id` no longer names.
    fn observe(&self, id: &uuid::Uuid, fact: Option<&ConnectionFact>) {
        self.memory
            .oauth_tokens
            .retain_generation(*id, fact.map(|fact| fact.active_generation));
        let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        let old = match fact {
            Some(fact) => seen.insert(*id, fact.clone()),
            None => seen.remove(id),
        };
        if let Some(credential) = old.and_then(|old| old.credential_id)
            && fact.and_then(|fact| fact.credential_id) != Some(credential)
        {
            self.secrets.invalidate(&credential);
        }
    }

    /// Read the deployment's connection map without a database read.
    pub fn release<'a>(
        &self,
        application: &'a wamn_catalog::application_config::ApplicationConfig,
    ) -> &'a wamn_catalog::application_config::ApplicationConfig {
        application
    }

    /// The map from integration name to connection id of the environment.
    pub async fn env_connections(&self) -> anyhow::Result<EnvConnections> {
        let map = EnvConnections {
            connections: self
                .reader
                .env_connections(&self.project, &self.environment)
                .await?,
        };
        Ok(map)
    }

    /// Generation `generation` of connection `id`.
    pub async fn generation(
        &self,
        id: &uuid::Uuid,
        generation: i64,
    ) -> anyhow::Result<Arc<ConnectionGeneration>> {
        let key = (*id, generation);
        if let Some(found) = self.generations.get(&key) {
            return Ok(found);
        }
        let found = Arc::new(
            self.reader
                .connection_generation(&self.project, &self.environment, id, generation)
                .await?
                .context("connection generation unavailable")?,
        );
        self.generations.insert(key, Arc::clone(&found));
        Ok(found)
    }

    /// The secret of credential `id`.
    pub async fn secret(&self, id: &uuid::Uuid) -> anyhow::Result<Arc<Secret>> {
        if let Some(found) = self.secrets.get(id) {
            return Ok(found);
        }
        let found = Arc::new(Secret(
            self.reader
                .credential_secret(&self.project, &self.environment, id)
                .await?
                .context("credential unavailable")?,
        ));
        self.secrets.insert(*id, Arc::clone(&found));
        Ok(found)
    }

    /// Version `version` of `integration_type`. Without a version, the
    /// newest published version, read from Postgres each time.
    pub async fn version(
        &self,
        integration_type: &wamn_registry::integration_code::IntegrationCode,
        version: Option<i32>,
    ) -> anyhow::Result<Arc<IntegrationVersion>> {
        if let Some(version) = version
            && let Some(found) = self.versions.get(&(integration_type.to_owned(), version))
        {
            return Ok(found);
        }
        let found = Arc::new(
            self.reader
                .integration_version(integration_type, version)
                .await?
                .context("integration version unavailable")?,
        );
        self.versions.insert(
            (integration_type.to_owned(), found.version),
            Arc::clone(&found),
        );
        Ok(found)
    }
}

impl ConnectionFacts {
    /// Check the deployment bindings against current project connection status.
    pub async fn release_component_bindings_ready(
        &self,
        application: &wamn_catalog::application_config::ApplicationConfig,
        component_digests: &[wamn_digest::Digest],
    ) -> anyhow::Result<bool> {
        for component in application
            .components
            .iter()
            .filter(|component| component_digests.contains(&component.digest))
        {
            for binding in &component.connections {
                let id = binding.connection_id.parse()?;
                if self.connection(&id).await?.is_none_or(|fact| {
                    fact.status != crate::connection_memory::ConnectionStatus::Active
                }) {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }
}
