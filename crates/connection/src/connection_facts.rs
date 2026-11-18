//! The connection facts of a call, from the connection memory and from
//! Postgres (docs/architecture/execution.md#connection-memory).
//!

use std::sync::Arc;

use anyhow::Context as _;

use crate::connection_credentials::{ConnectionCredentials, ConnectionGeneration, Secret};
use crate::connection_memory::{ConnectionFact, ConnectionMemory, EnvConnections};
use crate::database::ProjectDatabase;

/// The active connection facts and their generation.
#[derive(Debug)]
pub struct ConnectionAnswer {
    pub fact: ConnectionFact,
    pub generation: Arc<ConnectionGeneration>,
}

/// A generation row with its runtime type from this workload's declaration.
#[derive(Debug)]
pub struct Generation {
    pub runtime_type: &'static str,
    data: Arc<ConnectionGeneration>,
}

impl std::ops::Deref for Generation {
    type Target = ConnectionGeneration;
    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

/// Access to the host's single project-lifetime cache.
pub trait ConnectionCache: std::fmt::Debug + Send + Sync {
    fn bind_application(
        &self,
        workload: &str,
        application: &Arc<wamn_catalog::application_config::ApplicationConfig>,
    ) -> anyhow::Result<()>;
    fn get(
        &self,
        project: &wamn_registry::ProjectEnvKey,
        id: &uuid::Uuid,
    ) -> Option<Arc<ConnectionAnswer>>;
    fn insert(
        &self,
        project: &wamn_registry::ProjectEnvKey,
        id: uuid::Uuid,
        answer: Arc<ConnectionAnswer>,
    );
    fn invalidate(&self, project: &wamn_registry::ProjectEnvKey, id: &uuid::Uuid);
    fn integration(
        self: Arc<Self>,
        workload: &str,
        id: &uuid::Uuid,
    ) -> anyhow::Result<Arc<ConnectionFacts>>;
    fn project_facts(
        self: Arc<Self>,
        project: &wamn_registry::ProjectEnvKey,
    ) -> anyhow::Result<Arc<ConnectionFacts>>;
    fn facts(
        self: Arc<Self>,
        application: Arc<wamn_catalog::application_config::ApplicationConfig>,
    ) -> anyhow::Result<Arc<ConnectionFacts>>;
}

/// Project-owned connection reader, also used by incoming authentication and file routes.
#[derive(Debug)]
pub struct ConnectionFacts {
    oauth_encryption: Option<Arc<wamn_envelope::encryption::Envelope>>,
    memory: Arc<ConnectionMemory>,
    reader: ConnectionCredentials,
    project_env_key: wamn_registry::ProjectEnvKey,
    project: wamn_registry::ProjectId,
    environment: wamn_registry::EnvName,
    cache: Option<Arc<dyn ConnectionCache>>,
    application: Option<Arc<wamn_catalog::application_config::ApplicationConfig>>,
}

impl ConnectionFacts {
    pub fn new(
        memory: Arc<ConnectionMemory>,
        reader: Arc<ConnectionCredentials>,
        postgres: &Arc<impl ProjectDatabase + 'static>,
        project_env_key: &wamn_registry::ProjectEnvKey,
        project: &wamn_registry::ProjectId,
        environment: &wamn_registry::EnvName,
    ) -> Self {
        let database: std::sync::Weak<dyn ProjectDatabase> = Arc::downgrade(postgres) as _;
        Self {
            oauth_encryption: None,
            memory,
            reader: Arc::unwrap_or_clone(reader).with_project_database(
                database,
                project,
                environment,
            ),
            project_env_key: project_env_key.to_owned(),
            project: project.to_owned(),
            environment: environment.to_owned(),
            cache: None,
            application: None,
        }
    }
    pub fn with_cache(
        mut self,
        cache: Arc<dyn ConnectionCache>,
        application: Arc<wamn_catalog::application_config::ApplicationConfig>,
    ) -> Self {
        self.cache = Some(cache);
        self.application = Some(application);
        self
    }
    pub fn with_oauth_encryption(
        mut self,
        envelope: Option<Arc<wamn_envelope::encryption::Envelope>>,
    ) -> Self {
        self.oauth_encryption = envelope;
        self
    }
    pub fn oauth_encryption(&self) -> Option<Arc<wamn_envelope::encryption::Envelope>> {
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
    pub fn reader(&self) -> &ConnectionCredentials {
        &self.reader
    }
    pub fn credential_handle(&self, name: &str, generation: i64) -> anyhow::Result<String> {
        self.reader.connection_handle(name, generation)
    }

    /// Read connection metadata under one cache key, without a system read.
    pub async fn answer(&self, id: &uuid::Uuid) -> anyhow::Result<Option<Arc<ConnectionAnswer>>> {
        if let Some(answer) = self
            .cache
            .as_ref()
            .and_then(|cache| cache.get(&self.project_env_key, id))
        {
            return Ok(Some(answer));
        }
        let Some(fact) = self
            .reader
            .connection_fact(&self.project, &self.environment, id)
            .await?
        else {
            return Ok(None);
        };
        let generation = self
            .reader
            .connection_generation(&self.project, &self.environment, id, fact.active_generation)
            .await?
            .context("connection generation unavailable")?;
        self.memory
            .oauth_tokens
            .retain_generation(*id, Some(fact.active_generation));
        let answer = Arc::new(ConnectionAnswer {
            fact,
            generation: Arc::new(generation),
        });
        if answer.fact.status == crate::connection_memory::ConnectionStatus::Active
            && let Some(cache) = &self.cache
        {
            cache.insert(&self.project_env_key, *id, Arc::clone(&answer));
        }
        Ok(Some(answer))
    }
    pub async fn connection(&self, id: &uuid::Uuid) -> anyhow::Result<Option<ConnectionFact>> {
        Ok(self.answer(id).await?.map(|answer| answer.fact.clone()))
    }
    pub async fn read_again(
        &self,
        id: &uuid::Uuid,
    ) -> anyhow::Result<Option<Arc<ConnectionAnswer>>> {
        if let Some(cache) = &self.cache {
            cache.invalidate(&self.project_env_key, id);
        }
        self.answer(id).await
    }
    pub async fn env_connections(&self) -> anyhow::Result<EnvConnections> {
        Ok(EnvConnections {
            connections: self
                .reader
                .env_connections(&self.project, &self.environment)
                .await?,
        })
    }
    pub async fn generation(&self, id: &uuid::Uuid, generation: i64) -> anyhow::Result<Generation> {
        let data = match self.answer(id).await? {
            Some(answer) if answer.fact.active_generation == generation => {
                Arc::clone(&answer.generation)
            }
            _ => Arc::new(
                self.reader
                    .connection_generation(&self.project, &self.environment, id, generation)
                    .await?
                    .context("connection generation unavailable")?,
            ),
        };
        let connection_id = id.to_string();
        let runtime_type = self
            .application
            .as_ref()
            .and_then(|app| {
                app.components
                    .iter()
                    .flat_map(|component| &component.connections)
                    .find(|binding| binding.connection_id == connection_id)
            })
            .map_or("", |binding| match binding.runtime_type {
                wamn_catalog::RuntimeType::Host => "host",
                wamn_catalog::RuntimeType::Wasm => "wasm",
                wamn_catalog::RuntimeType::Native => "native",
            });
        Ok(Generation { runtime_type, data })
    }
    /// Read the credential selected by a connection fact.
    pub async fn secret(
        &self,
        id: uuid::Uuid,
        rejected: Option<&Arc<Secret>>,
    ) -> anyhow::Result<Arc<Secret>> {
        self.reader
            .credential_secret(
                &self.project_env_key,
                &self.project,
                &self.environment,
                id,
                rejected,
            )
            .await
    }
}
