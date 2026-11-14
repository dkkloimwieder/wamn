//! Host-owned connection credentials read through login-targeted database functions.
//!
//! Secret material never enters guests or stored run facts. An absent reader
//! is unavailable. A reader with no matching enabled connection returns none.

use sqlx::Row as _;
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

/// Connection credentials targeted by the authenticated reader login.
#[derive(Clone)]
pub struct ConnectionCredentials {
    org: Option<String>,
    store: Option<Arc<dyn wamn_kubernetes::Store>>,
    database: Option<sqlx::PgPool>,
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
    /// Use a dedicated system reader whose login fixes its org and project.
    pub fn from_database(client: sqlx::PgPool, org: &str) -> Self {
        Self {
            org: Some(org.to_owned()),
            store: None,
            database: Some(client),
        }
    }

    pub async fn oauth_database(
        &self,
    ) -> anyhow::Result<sqlx::pool::PoolConnection<sqlx::Postgres>> {
        self.database
            .as_ref()
            .context("OAuth database unavailable")?
            .acquire()
            .await
            .context("open OAuth transaction connection")
    }

    pub fn org(&self) -> anyhow::Result<&str> {
        self.org.as_deref().context("connection reader has no org")
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
        }
    }

    /// Build a pinned handle from this reader's organization.
    pub fn connection_handle(&self, name: &str, generation: i64) -> anyhow::Result<String> {
        let org = self
            .org
            .as_deref()
            .context("connection credentials unavailable")?;
        let handle = format!("org:{org}:{name}@{generation}");
        self.connection_name(&handle)?;
        Ok(handle)
    }

    async fn material(&self, row: &sqlx::postgres::PgRow) -> anyhow::Result<Option<String>> {
        if row.get::<Option<&str>, _>(0).is_none() {
            return Ok(None);
        }
        let reference: String = row.get(1);
        let version: String = row.get(2);
        Ok(Some(
            read_secret(&reference, &version, self.store.as_deref()).await?,
        ))
    }

    pub fn connection_name<'a>(&self, handle: &'a str) -> anyhow::Result<&'a str> {
        let (org, name) =
            wamn_catalog::parse_org_credential_handle(handle).map_err(anyhow::Error::msg)?;
        let configured_org = self
            .org
            .as_deref()
            .context("connection credentials unavailable")?;
        anyhow::ensure!(configured_org == org, "credential org mismatch");
        Ok(name)
    }
}

/// The connection memory reads (docs/architecture/execution.md#connection-memory).
/// Every read but [`ConnectionCredentials::connection_fact`] and
/// [`ConnectionCredentials::env_connections`] returns rows that never change
/// under their keys.
impl ConnectionCredentials {
    async fn reader(&self) -> anyhow::Result<(sqlx::pool::PoolConnection<sqlx::Postgres>, &str)> {
        let client = self
            .database
            .as_ref()
            .context("connection reader unavailable")?
            .acquire()
            .await?;
        let org = self
            .org
            .as_deref()
            .context("connection reader has no org")?;
        Ok((client, org))
    }

    /// The active generation, status, credential, principal and lease coordinates of one
    /// connection of the environment.
    pub async fn connection_fact(
        &self,
        project: &str,
        environment: &str,
        id: &uuid::Uuid,
    ) -> anyhow::Result<Option<crate::connection_memory::ConnectionFact>> {
        let (mut client, org) = self.reader().await?;
        let row = sqlx::query(
            wamn_registry::sql::PLATFORM_RUNTIME_SRC_CONNECTION_CREDENTIALS_CONNECTION_FACT,
        )
        .bind(org)
        .bind(project)
        .bind(environment)
        .bind(id)
        .fetch_optional(&mut *client)
        .await
        .context("read the connection fact")?;
        Ok(row.map(|row| crate::connection_memory::ConnectionFact {
            active_generation: row.get(0),
            status: if row.get(1) {
                crate::connection_memory::ConnectionStatus::Active
            } else {
                crate::connection_memory::ConnectionStatus::Disabled
            },
            credential_id: row.get(2),
            exclusive: row.get(3),
            target_id: row.get(4),
            resource_key: row.get(5),
            target_type: row.get(6),
            service_principal_id: row.get(7),
        }))
    }

    /// One generation of one connection of the environment.
    pub async fn connection_generation(
        &self,
        project: &str,
        environment: &str,
        id: &uuid::Uuid,
        generation: i64,
    ) -> anyhow::Result<Option<ConnectionGeneration>> {
        let (mut client, org) = self.reader().await?;
        let row = sqlx::query(
            wamn_registry::sql::PLATFORM_RUNTIME_SRC_CONNECTION_CREDENTIALS_CONNECTION_GENERATION,
        )
        .bind(org)
        .bind(project)
        .bind(environment)
        .bind(id)
        .bind(generation)
        .fetch_optional(&mut *client)
        .await
        .context("read the connection generation")?;
        Ok(row.map(|row| ConnectionGeneration {
            name: row.get(0),
            integration_type: row.get(1),
            runtime_type: row.get(2),
            auth_type: row.get(3),
            external_system: row.get(4),
            parameters: row.get(5),
        }))
    }

    /// The secret of one credential of a connection of the environment.
    pub async fn credential_secret(
        &self,
        project: &str,
        environment: &str,
        credential: &uuid::Uuid,
    ) -> anyhow::Result<Option<String>> {
        let (mut client, org) = self.reader().await?;
        let row = sqlx::query(
            wamn_registry::sql::PLATFORM_RUNTIME_SRC_CONNECTION_CREDENTIALS_CREDENTIAL_SECRET,
        )
        .bind(org)
        .bind(project)
        .bind(environment)
        .bind(credential)
        .fetch_optional(&mut *client)
        .await
        .context("read the credential reference")?;
        match row {
            Some(row) => self.material(&row).await,
            None => Ok(None),
        }
    }

    /// The map from integration name to connection id of the environment.
    pub async fn env_connections(
        &self,
        project: &str,
        environment: &str,
    ) -> anyhow::Result<std::collections::BTreeMap<String, uuid::Uuid>> {
        let (mut client, org) = self.reader().await?;
        Ok(sqlx::query(
            wamn_registry::sql::PLATFORM_RUNTIME_SRC_CONNECTION_CREDENTIALS_ENV_CONNECTIONS,
        )
        .bind(org)
        .bind(project)
        .bind(environment)
        .fetch_all(&mut *client)
        .await
        .context("read the environment connections")?
        .into_iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect())
    }

    /// One published version of an integration type, or its newest one.
    pub async fn integration_version(
        &self,
        integration_type: &str,
        version: Option<i32>,
    ) -> anyhow::Result<Option<IntegrationVersion>> {
        let (mut client, _) = self.reader().await?;
        let row = sqlx::query(
            wamn_registry::sql::PLATFORM_RUNTIME_SRC_CONNECTION_CREDENTIALS_INTEGRATION_VERSION,
        )
        .bind(integration_type)
        .bind(version)
        .fetch_optional(&mut *client)
        .await
        .context("read the integration version")?;
        Ok(row.map(|row| IntegrationVersion {
            version: row.get(0),
            digest: row.get(1),
        }))
    }
}

/// One generation of a connection. The secret stays apart, under the
/// credential id.
#[derive(Clone, Debug)]
pub struct ConnectionGeneration {
    pub name: String,
    pub integration_type: String,
    pub runtime_type: String,
    pub auth_type: String,
    pub external_system: Option<String>,
    pub parameters: serde_json::Value,
}

/// One published version of an integration type.
#[derive(Clone, Debug)]
pub struct IntegrationVersion {
    pub version: i32,
    pub digest: Option<String>,
}

#[cfg(any(test, feature = "test-util"))]
pub mod test_util;

#[cfg(test)]
mod tests {
    use super::*;
    use test_util::Fixture;

    #[tokio::test]
    async fn active_references_are_targeted_and_disabled_connections_refuse() {
        let _lock = wamn_test_postgres::lock();
        let mut fixture = Fixture::new().await;
        fixture.put("erp", "first", &["project-a"]).await;
        fixture.put("erp", "other", &["project-b"]).await;
        fixture.put("erp", "current", &["project-a"]).await;
        let reader = fixture.reader("project-a").await;
        let id = reader.env_connections("project-a", "dev").await.unwrap()["erp"];
        let fact = reader
            .connection_fact("project-a", "dev", &id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            reader
                .credential_secret("project-a", "dev", &fact.credential_id.unwrap())
                .await
                .unwrap()
                .as_deref(),
            Some("current")
        );
        assert!(
            reader
                .env_connections("project-b", "dev")
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(reader.connection_name("org:acme:erp@2").unwrap(), "erp");
        assert!(reader.connection_name("org:other:erp").is_err());
        assert!(reader.connection_name("project:project-a:erp").is_err());
        let principal: uuid::Uuid =
            sqlx::query("SELECT service_principal_id FROM registry.connection WHERE id=$1")
                .bind(&id)
                .fetch_one(&mut fixture.admin)
                .await
                .unwrap()
                .get(0);
        assert_eq!(fact.service_principal_id, principal);
        assert!(
            sqlx::raw_sql("SELECT * FROM identity.credential")
                .fetch_all(reader.database.as_ref().unwrap())
                .await
                .is_err()
        );
        sqlx::raw_sql("SET ROLE wamn_system; UPDATE registry.integration SET status='disabled' WHERE project='project-a'; RESET ROLE").execute(&mut fixture.admin).await.map(drop).unwrap();
        assert_eq!(
            reader
                .connection_fact("project-a", "dev", &id)
                .await
                .unwrap()
                .unwrap()
                .status,
            crate::connection_memory::ConnectionStatus::Disabled
        );
    }

    #[tokio::test]
    async fn memory_reads_follow_the_active_generation_inside_the_login_environment() {
        use crate::connection_memory::ConnectionStatus;

        let _lock = wamn_test_postgres::lock();
        let mut fixture = Fixture::new().await;
        fixture.put_store("files", "first", &["project-a"]).await;
        fixture.put("erp", "http", &["project-a"]).await;
        let reader = fixture.reader("project-a").await;
        let map = reader.env_connections("project-a", "dev").await.unwrap();
        assert_eq!(
            map.keys().map(String::as_str).collect::<Vec<_>>(),
            ["erp", "files"]
        );
        let store = map["files"];
        let first = reader
            .connection_fact("project-a", "dev", &store)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.status, ConnectionStatus::Active);
        let first_credential = first.credential_id.unwrap();
        fixture.put_store("files", "current", &["project-a"]).await;
        let current = reader
            .connection_fact("project-a", "dev", &store)
            .await
            .unwrap()
            .unwrap();
        assert!(current.active_generation > first.active_generation);
        assert_ne!(current.credential_id, Some(first_credential));
        // A generation and a credential never change under their ids.
        let generation = reader
            .connection_generation("project-a", "dev", &store, first.active_generation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(generation.name, "files");
        assert_eq!(generation.integration_type, "s3");
        assert_eq!(generation.runtime_type, "host");
        assert_eq!(generation.parameters["container"], "files");
        assert_eq!(
            reader
                .credential_secret("project-a", "dev", &first_credential)
                .await
                .unwrap()
                .as_deref(),
            Some("first")
        );
        assert_eq!(
            reader
                .credential_secret("project-a", "dev", &current.credential_id.unwrap())
                .await
                .unwrap()
                .as_deref(),
            Some("current")
        );
        // Another project of the login reads nothing.
        assert!(
            reader
                .connection_fact("project-b", "dev", &store)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            reader
                .credential_secret("project-b", "dev", &first_credential)
                .await
                .unwrap()
                .is_none()
        );
        sqlx::raw_sql("SET ROLE wamn_system; UPDATE registry.integration SET status='disabled' WHERE project='project-a' AND name='files'; RESET ROLE").execute(&mut fixture.admin).await.map(drop).unwrap();
        assert_eq!(
            reader
                .connection_fact("project-a", "dev", &store)
                .await
                .unwrap()
                .unwrap()
                .status,
            ConnectionStatus::Disabled
        );
    }

    #[tokio::test]
    async fn a_generation_carries_its_authentication() {
        use serde_json::json;

        let _lock = wamn_test_postgres::lock();
        let mut fixture = Fixture::new().await;
        let parameters =
            json!({"client_id":"app","token_url":"https://example.com/token","scopes":["read"]});
        fixture
            .put_http(
                "erp",
                r#"{"client_secret":"first"}"#,
                &["project-a"],
                ("oauth2_client_credentials", parameters.clone()),
            )
            .await;
        let reader = fixture.reader("project-a").await;
        let id = reader.env_connections("project-a", "dev").await.unwrap()["erp"];
        let fact = reader
            .connection_fact("project-a", "dev", &id)
            .await
            .unwrap()
            .unwrap();
        let generation = reader
            .connection_generation("project-a", "dev", &id, fact.active_generation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(generation.auth_type, "oauth2_client_credentials");
        assert_eq!(generation.parameters["auth"], parameters);
        assert!(generation.parameters["auth"].get("client_secret").is_none());
        assert_eq!(
            reader
                .credential_secret("project-a", "dev", &fact.credential_id.unwrap())
                .await
                .unwrap()
                .as_deref(),
            Some(r#"{"client_secret":"first"}"#)
        );
        let http_only: bool =
            sqlx::query("SELECT http_only FROM registry.auth_type WHERE code='hmac'")
                .fetch_one(&mut fixture.admin)
                .await
                .unwrap()
                .get(0);
        assert!(http_only);
    }

    #[tokio::test]
    async fn an_absent_reader_is_unavailable() {
        assert!(
            ConnectionCredentials::empty()
                .connection_fact("project-a", "dev", &uuid::Uuid::new_v4())
                .await
                .is_err()
        );
    }
}
