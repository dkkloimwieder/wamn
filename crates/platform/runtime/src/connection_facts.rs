//! The connection facts of a call, from the connection memory and from
//! Postgres (docs/architecture/execution.md#connection-memory).
//!
//! The mutable facts come from [`ConnectionMemory`]: the fact of a
//! connection, the connection map of a release and the connection map of the
//! environment. A miss reads Postgres and puts the fact with its ticket. The
//! immutable rows stay in bounded maps of this host under their own ids: a
//! generation under (connection, generation), a version under (type,
//! version) and a secret under its credential id. A new fact of a connection
//! drops the secret that no current fact names.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use anyhow::Context as _;
use moka::policy::EvictionPolicy;
use moka::sync::Cache;
use wamn_platform_identity::connection_changes::{ConnectionChanges, ConnectionDrops};

use crate::connection_credentials::{
    ConnectionCredentials, ConnectionGeneration, IntegrationVersion,
};
use crate::connection_memory::{ConnectionFact, ConnectionMemory, EnvConnections, ReleaseFact};
use crate::plugins::wamn_postgres::WamnPostgres;

#[cfg(test)]
mod tests;

/// The number of entries of each bounded map.
pub const MAP_ENTRIES: u64 = 1024;

/// A bounded map that evicts its least recently used entry.
pub(crate) fn bounded<K, V>() -> Cache<K, V>
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

/// The connection facts one host reads for the calls of one environment.
pub struct ConnectionFacts {
    memory: Arc<ConnectionMemory>,
    changes: ConnectionChanges,
    reader: Arc<ConnectionCredentials>,
    postgres: Arc<WamnPostgres>,
    env_key: String,
    project: String,
    environment: String,
    generations: Cache<(uuid::Uuid, i64), Arc<ConnectionGeneration>>,
    versions: Cache<(String, i32), Arc<IntegrationVersion>>,
    secrets: Cache<uuid::Uuid, Arc<Secret>>,
    /// The last fact this host saw of each connection.
    seen: Mutex<HashMap<uuid::Uuid, ConnectionFact>>,
}

impl std::fmt::Debug for ConnectionFacts {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConnectionFacts")
            .field("env_key", &self.env_key)
            .finish_non_exhaustive()
    }
}

impl ConnectionFacts {
    pub fn new(
        memory: Arc<ConnectionMemory>,
        changes: ConnectionChanges,
        reader: Arc<ConnectionCredentials>,
        postgres: Arc<WamnPostgres>,
        env_key: &str,
        project: &str,
        environment: &str,
    ) -> Self {
        Self {
            memory,
            changes,
            reader,
            postgres,
            env_key: env_key.to_owned(),
            project: project.to_owned(),
            environment: environment.to_owned(),
            generations: bounded(),
            versions: bounded(),
            secrets: bounded(),
            seen: Mutex::new(HashMap::new()),
        }
    }

    pub fn env_key(&self) -> &str {
        &self.env_key
    }

    /// The reader of the facts, for the local application.
    pub(crate) fn reader(&self) -> Arc<ConnectionCredentials> {
        Arc::clone(&self.reader)
    }

    /// The credential handle of generation `generation` of connection `name`.
    pub(crate) fn credential_handle(&self, name: &str, generation: i64) -> anyhow::Result<String> {
        self.reader.connection_handle(name, generation)
    }

    /// The fact of connection `id`, or `None` when the environment has no
    /// such connection.
    pub async fn connection(&self, id: &uuid::Uuid) -> anyhow::Result<Option<ConnectionFact>> {
        let fact = match self.memory.connection(&self.env_key, id) {
            Ok(fact) => Some(fact),
            Err(ticket) => {
                let fact = self
                    .reader
                    .connection_fact(&self.project, &self.environment, id)
                    .await?;
                if let Some(fact) = &fact {
                    self.memory
                        .keep_connection(&self.env_key, id, ticket, fact.clone())
                        .await;
                }
                fact
            }
        };
        self.observe(id, fact.as_ref());
        Ok(fact)
    }

    /// Drop the fact of connection `id` from every host and its secret from
    /// this one, and read the fact again from Postgres. A sender calls this
    /// once after the external system refuses the credential.
    pub async fn read_again(&self, id: &uuid::Uuid) -> anyhow::Result<Option<ConnectionFact>> {
        let mut drops = ConnectionDrops::default();
        drops.connection(&self.env_key, *id);
        self.changes.publish(drops).await;
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

    /// The connection map of the release `manifest_digest`.
    pub async fn release(&self, manifest_digest: &str) -> anyhow::Result<ReleaseFact> {
        match self.memory.release(&self.env_key, manifest_digest) {
            Ok(fact) => Ok(fact),
            Err(ticket) => {
                let fact = self
                    .postgres
                    .release_fact(&self.project, &self.env_key, manifest_digest)
                    .await?;
                self.memory
                    .keep_release(&self.env_key, manifest_digest, ticket, fact.clone())
                    .await;
                Ok(fact)
            }
        }
    }

    /// The map from integration name to connection id of the environment.
    pub async fn env_connections(&self) -> anyhow::Result<EnvConnections> {
        match self.memory.connections(&self.env_key) {
            Ok(map) => Ok(map),
            Err(ticket) => {
                let map = EnvConnections {
                    connections: self
                        .reader
                        .env_connections(&self.project, &self.environment)
                        .await?,
                };
                self.memory
                    .keep_connections(&self.env_key, ticket, map.clone())
                    .await;
                Ok(map)
            }
        }
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
        integration_type: &str,
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
