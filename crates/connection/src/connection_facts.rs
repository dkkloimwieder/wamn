//! The connection facts of a call, from the connection memory and from
//! Postgres (docs/architecture/execution.md#connection-memory).
//!

use std::sync::Arc;

use anyhow::Context as _;

use crate::connection_credentials::{ConnectionCredentials, ConnectionGeneration, Secret};
use crate::connection_memory::{ConnectionFact, ConnectionMemory, EnvConnections};
use wamn_postgres::WamnPostgres;

/// The active connection facts and their generation.
#[derive(Debug)]
pub struct ConnectionAnswer {
    pub fact: ConnectionFact,
    pub generation: Arc<ConnectionGeneration>,
}

/// Access to the host's single project-lifetime cache.
pub trait ConnectionCache: std::fmt::Debug + Send + Sync {
    fn bind_app(
        &self,
        workload: &str,
        app: &Arc<wamn_app::app_config::AppConfig>,
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
        app: Arc<wamn_app::app_config::AppConfig>,
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
    env: wamn_registry::EnvName,
    cache: Option<Arc<dyn ConnectionCache>>,
    app: Option<Arc<wamn_app::app_config::AppConfig>>,
}

impl ConnectionFacts {
    pub fn new(
        memory: Arc<ConnectionMemory>,
        reader: Arc<ConnectionCredentials>,
        database: &Arc<WamnPostgres>,
        project_env_key: &wamn_registry::ProjectEnvKey,
        project: &wamn_registry::ProjectId,
        env: &wamn_registry::EnvName,
    ) -> Self {
        let database = Arc::downgrade(database);
        Self {
            oauth_encryption: None,
            memory,
            reader: Arc::unwrap_or_clone(reader).with_project_database(database, project, env),
            project_env_key: project_env_key.to_owned(),
            project: project.to_owned(),
            env: env.to_owned(),
            cache: None,
            app: None,
        }
    }
    pub fn with_cache(
        mut self,
        cache: Arc<dyn ConnectionCache>,
        app: Arc<wamn_app::app_config::AppConfig>,
    ) -> Self {
        self.cache = Some(cache);
        self.app = Some(app);
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
    pub fn env(&self) -> &wamn_registry::EnvName {
        &self.env
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

    /// Read the live connection fact before reusing cached generation metadata.
    pub async fn answer(&self, id: &uuid::Uuid) -> anyhow::Result<Option<Arc<ConnectionAnswer>>> {
        let Some(fact) = self
            .reader
            .connection_fact(&self.project, &self.env, id)
            .await?
        else {
            return Ok(None);
        };
        if let Some(answer) = self
            .cache
            .as_ref()
            .and_then(|cache| cache.get(&self.project_env_key, id))
            .filter(|answer| answer.fact == fact)
        {
            return Ok(Some(answer));
        }
        let generation = self
            .reader
            .connection_generation(&self.project, &self.env, id, fact.active_generation)
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
                .env_connections(&self.project, &self.env)
                .await?,
        })
    }
    /// The bound runtime type from the workload's declaration.
    pub fn runtime_type(&self, id: &uuid::Uuid) -> Option<wamn_connection_type::RuntimeType> {
        let connection_id = id.to_string();
        self.app
            .as_ref()?
            .components
            .iter()
            .flat_map(|component| &component.connections)
            .find(|binding| binding.connection_id == connection_id)
            .map(|binding| binding.runtime_type)
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
                &self.env,
                id,
                rejected,
            )
            .await
    }
}
