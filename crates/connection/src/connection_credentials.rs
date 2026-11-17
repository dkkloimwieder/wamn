//! Connection credentials in project databases, with system-owned integration types.
//!
//! Secret material never enters guests or stored run facts. Project pools select
//! connection data; the system pool supplies types, targets and principal status.

use std::sync::Arc;

use anyhow::Context as _;

/// Read one versioned credential through the configured store.
pub async fn read_secret(
    reference: &str,
    version: &str,
    store: Option<&dyn wamn_kubernetes::Store>,
) -> anyhow::Result<String> {
    let configured;
    let backend = if let Some(backend) = store {
        backend
    } else {
        configured =
            wamn_kubernetes::kubernetes::KubernetesStore::in_cluster().map_err(|error| {
                let local = error.type_ == Some(wamn_kubernetes::ErrorType::NoCluster);
                let error = anyhow::Error::new(error);
                if local {
                    let reason = "no credential store in local mode";
                    tracing::warn!("{reason}");
                    error.context(reason)
                } else {
                    error
                }
            })?;
        &configured
    };
    String::from_utf8(backend.get(reference, version).await?).context("credential is not UTF-8")
}

#[derive(Clone)]
struct ProjectReader {
    database: std::sync::Weak<dyn crate::database::ProjectDatabase>,
    project: wamn_registry::ProjectId,
    environment: wamn_registry::EnvName,
}

/// Connection credentials selected through the project platform pool.
#[derive(Clone)]
pub struct ConnectionCredentials {
    org: Option<wamn_registry::OrgId>,
    store: Option<Arc<dyn wamn_kubernetes::Store>>,
    database: Option<sqlx::PgPool>,
    project: Option<ProjectReader>,
}

impl std::fmt::Debug for ConnectionCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConnectionCredentials")
            .field("available", &self.database.is_some())
            .finish_non_exhaustive()
    }
}

impl ConnectionCredentials {
    /// Use the system pool for integration type metadata.
    pub fn from_database(client: sqlx::PgPool, org: &wamn_registry::OrgId) -> Self {
        Self {
            org: Some(org.to_owned()),
            store: None,
            database: Some(client),
            project: None,
        }
    }

    pub(crate) fn with_project_database(
        mut self,
        database: std::sync::Weak<dyn crate::database::ProjectDatabase>,
        project: &wamn_registry::ProjectId,
        environment: &wamn_registry::EnvName,
    ) -> Self {
        self.project = Some(ProjectReader {
            database,
            project: project.to_owned(),
            environment: environment.to_owned(),
        });
        self
    }

    fn project_database(
        &self,
        project: &wamn_registry::ProjectId,
        environment: &wamn_registry::EnvName,
    ) -> anyhow::Result<Arc<dyn crate::database::ProjectDatabase>> {
        let target = self
            .project
            .as_ref()
            .context("project database unavailable")?;
        anyhow::ensure!(
            &target.project == project && &target.environment == environment,
            "connection project mismatch"
        );
        target
            .database
            .upgrade()
            .context("project database was unloaded")
    }

    pub async fn oauth_database(
        &self,
        project: &wamn_registry::ProjectId,
        environment: &wamn_registry::EnvName,
    ) -> anyhow::Result<sqlx::pool::PoolConnection<sqlx::Postgres>> {
        self.project_database(project, environment)?
            .oauth_connection(project)
            .await
    }

    pub fn org(&self) -> anyhow::Result<&wamn_registry::OrgId> {
        self.org.as_ref().context("connection reader has no org")
    }

    /// Supply the environment store, including isolated test fixtures.
    pub fn with_store(mut self, store: Arc<dyn wamn_kubernetes::Store>) -> Self {
        self.store = Some(store);
        self
    }

    /// Keep the explicit unavailable state for hosts without a credential reader.
    pub fn empty() -> Self {
        Self {
            org: None,
            store: None,
            database: None,
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
        let (org, name) =
            wamn_catalog::parse_org_credential_handle(handle).map_err(anyhow::Error::msg)?;
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
    async fn reader(&self) -> anyhow::Result<sqlx::Transaction<'_, sqlx::Postgres>> {
        let mut tx = self
            .database
            .as_ref()
            .context("system reader unavailable")?
            .begin()
            .await?;
        sqlx::query(
            "INSERT INTO platform.tx_identity(principal_id,role,application) VALUES($1,'','')",
        )
        .bind(wamn_identity_protocol::PlatformComponent::Executor.principal_id())
        .execute(&mut *tx)
        .await?;
        Ok(tx)
    }

    /// The active generation, status, credential, principal and lease coordinates of one
    /// connection of the environment.
    pub async fn connection_fact(
        &self,
        project: &wamn_registry::ProjectId,
        environment: &wamn_registry::EnvName,
        id: &uuid::Uuid,
    ) -> anyhow::Result<Option<crate::connection_memory::ConnectionFact>> {
        let database = self.project_database(project, environment)?;
        let mut local = database.read_connection(project).await?;
        let mut system = self.reader().await?;
        let row = wamn_registry::connections::fact(
            local.connection(),
            &mut system,
            self.org()?,
            project,
            environment,
            id,
        )
        .await
        .context("read connection fact")?;
        local.commit().await?;
        system.commit().await?;
        Ok(row.map(|row| crate::connection_memory::ConnectionFact {
            active_generation: row.active_generation,
            status: if row.active {
                crate::connection_memory::ConnectionStatus::Active
            } else {
                crate::connection_memory::ConnectionStatus::Disabled
            },
            credential_id: row.credential_id,
            exclusive: row.exclusive,
            target_id: row.target_id,
            resource_key: row.resource_key,
            target_type: row.target_type,
            service_principal_id: row.service_principal_id,
        }))
    }

    /// One generation of one connection of the environment.
    pub async fn connection_generation(
        &self,
        project: &wamn_registry::ProjectId,
        environment: &wamn_registry::EnvName,
        id: &uuid::Uuid,
        generation: i64,
    ) -> anyhow::Result<Option<ConnectionGeneration>> {
        let database = self.project_database(project, environment)?;
        let mut local = database.read_connection(project).await?;
        let mut system = self.reader().await?;
        let row =
            wamn_registry::connections::generation(local.connection(), &mut system, id, generation)
                .await
                .context("read connection generation")?;
        local.commit().await?;
        system.commit().await?;
        Ok(row)
    }

    /// The secret of one credential of a connection of the environment.
    pub async fn credential_secret(
        &self,
        project: &wamn_registry::ProjectId,
        environment: &wamn_registry::EnvName,
        credential: &uuid::Uuid,
    ) -> anyhow::Result<Option<String>> {
        let database = self.project_database(project, environment)?;
        let mut local = database.read_connection(project).await?;
        let reference = wamn_registry::connections::credential(local.connection(), credential)
            .await
            .context("read credential reference")?;
        local.commit().await?;
        match reference {
            Some(reference) => Ok(Some(
                read_secret(
                    &reference.reference,
                    &reference.version,
                    self.store.as_deref(),
                )
                .await?,
            )),
            None => Ok(None),
        }
    }

    /// The map from integration name to connection id of the environment.
    pub async fn env_connections(
        &self,
        project: &wamn_registry::ProjectId,
        environment: &wamn_registry::EnvName,
    ) -> anyhow::Result<std::collections::BTreeMap<String, uuid::Uuid>> {
        let database = self.project_database(project, environment)?;
        let mut local = database.read_connection(project).await?;
        let rows = wamn_registry::connections::environment(local.connection())
            .await
            .context("read environment connections")?;
        local.commit().await?;
        Ok(rows)
    }

    /// One published version of an integration type, or its newest one.
    pub async fn integration_version(
        &self,
        integration_type: &wamn_registry::integration_code::IntegrationCode,
        version: Option<i32>,
    ) -> anyhow::Result<Option<IntegrationVersion>> {
        let mut client = self
            .database
            .as_ref()
            .context("system reader unavailable")?
            .acquire()
            .await?;
        wamn_registry::connections::integration_version(&mut client, integration_type, version)
            .await
            .context("read the integration version")
    }
}

pub use wamn_registry::connections::{ConnectionGeneration, IntegrationVersion};

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
