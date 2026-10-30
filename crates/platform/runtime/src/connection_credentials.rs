//! Host-owned connection credentials read through login-scoped database functions.
//!
//! Secret material never enters guests or stored run facts. An absent reader
//! is unavailable. A reader with no matching enabled connection returns none.

use std::sync::Arc;

use anyhow::Context as _;

/// Connection credentials scoped by the authenticated reader login.
#[derive(Clone)]
pub struct ConnectionCredentials {
    org: Option<String>,
    store: Option<Arc<dyn wamn_credential_store::Store>>,
    database: Option<Arc<tokio_postgres::Client>>,
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
    pub fn from_database(client: Arc<tokio_postgres::Client>, org: &str) -> Self {
        Self {
            org: Some(org.to_owned()),
            store: None,
            database: Some(client),
        }
    }

    /// Supply the environment store, including in tests with private files.
    pub fn with_store(mut self, store: Arc<dyn wamn_credential_store::Store>) -> Self {
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
    pub(crate) fn connection_handle(&self, name: &str, generation: i64) -> anyhow::Result<String> {
        let org = self
            .org
            .as_deref()
            .context("connection credentials unavailable")?;
        let handle = format!("org:{org}:{name}@{generation}");
        self.handle(&handle)?;
        Ok(handle)
    }

    async fn connection(
        &self,
        project: &str,
        name: &str,
    ) -> anyhow::Result<Option<tokio_postgres::Row>> {
        let client = self
            .database
            .as_ref()
            .context("connection credentials unavailable")?;
        let org = self
            .org
            .as_deref()
            .context("credential reader has no org")?;
        client.query_opt(
            "SELECT store, ref, version, principal_id, external_system, auth_type, parameters FROM registry.read_connection_credential($1,$2,$3)",
            &[&org, &project, &name],
        ).await.context("read connection credential authority")
    }

    /// Read the active generation through the environment credential store.
    pub async fn read_connection(
        &self,
        project: &str,
        handle: &str,
    ) -> anyhow::Result<Option<String>> {
        let (name, _) = self.handle(handle)?;
        let Some(row) = self.connection(project, name).await? else {
            return Ok(None);
        };
        self.material(&row).await
    }

    async fn material(&self, row: &tokio_postgres::Row) -> anyhow::Result<Option<String>> {
        let Some(store) = row.get::<_, Option<String>>(0) else {
            return Ok(None);
        };
        let reference: String = row.get(1);
        let version: String = row.get(2);
        let backend = match &self.store {
            Some(backend) => std::sync::Arc::clone(backend),
            None => std::sync::Arc::from(wamn_credential_store::open(&store)?),
        };
        let value = backend.get(&reference, &version).await?;
        Ok(Some(
            String::from_utf8(value).context("credential is not UTF-8")?,
        ))
    }

    /// Resolve a connection principal through the same authority as its credential.
    pub async fn inbound_principal(
        &self,
        project: &str,
        handle: &str,
    ) -> anyhow::Result<Option<String>> {
        let (name, _) = self.handle(handle)?;
        Ok(self
            .connection(project, name)
            .await?
            .and_then(|row| row.get(3)))
    }

    /// Resolve an inbound service principal by its connection identity.
    pub async fn principal_for_connection(
        &self,
        project: &str,
        connection: uuid::Uuid,
    ) -> anyhow::Result<Option<String>> {
        let client = self
            .database
            .as_ref()
            .context("connection credentials unavailable")?;
        let org = self
            .org
            .as_deref()
            .context("credential reader has no org")?;
        Ok(client.query_opt("SELECT principal_id FROM registry.read_connection_credential($1,$2,NULL,$3::text::uuid)",
            &[&org, &project, &connection.to_string()]).await?.and_then(|row| row.get(0)))
    }

    fn handle<'a>(&self, handle: &'a str) -> anyhow::Result<(&'a str, Option<i64>)> {
        let (org, name) =
            wamn_catalog::parse_org_credential_handle(handle).map_err(anyhow::Error::msg)?;
        let configured_org = self
            .org
            .as_deref()
            .context("connection credentials unavailable")?;
        anyhow::ensure!(configured_org == org, "credential org mismatch");
        let generation = handle
            .rsplit_once('@')
            .map(|(_, value)| value.parse())
            .transpose()?;
        Ok((name, generation))
    }
}

/// The connection memory reads (docs/architecture/execution.md#connection-memory).
/// Every read but [`ConnectionCredentials::connection_fact`] and
/// [`ConnectionCredentials::env_connections`] returns rows that never change
/// under their keys.
impl ConnectionCredentials {
    fn reader(&self) -> anyhow::Result<(&tokio_postgres::Client, &str)> {
        let client = self
            .database
            .as_deref()
            .context("connection reader unavailable")?;
        let org = self
            .org
            .as_deref()
            .context("connection reader has no org")?;
        Ok((client, org))
    }

    /// The active generation, effective status and credential of one
    /// connection of the environment.
    pub(crate) async fn connection_fact(
        &self,
        project: &str,
        environment: &str,
        id: &uuid::Uuid,
    ) -> anyhow::Result<Option<crate::connection_memory::ConnectionFact>> {
        let (client, org) = self.reader()?;
        let row = client
            .query_opt(
                "SELECT active_generation, active, credential_id FROM registry.read_connection_fact($1,$2,$3,$4)",
                &[&org, &project, &environment, id],
            )
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
        }))
    }

    /// One generation of one connection of the environment.
    pub(crate) async fn connection_generation(
        &self,
        project: &str,
        environment: &str,
        id: &uuid::Uuid,
        generation: i64,
    ) -> anyhow::Result<Option<ConnectionGeneration>> {
        let (client, org) = self.reader()?;
        let row = client
            .query_opt(
                "SELECT name, integration_type, runtime_type, auth_type, external_system, principal_id, parameters FROM registry.read_connection_generation($1,$2,$3,$4,$5)",
                &[&org, &project, &environment, id, &generation],
            )
            .await
            .context("read the connection generation")?;
        Ok(row.map(|row| ConnectionGeneration {
            name: row.get(0),
            integration_type: row.get(1),
            runtime_type: row.get(2),
            auth_type: row.get(3),
            external_system: row.get(4),
            principal_id: row.get(5),
            parameters: row.get(6),
        }))
    }

    /// The secret of one credential of a connection of the environment.
    pub(crate) async fn credential_secret(
        &self,
        project: &str,
        environment: &str,
        credential: &uuid::Uuid,
    ) -> anyhow::Result<Option<String>> {
        let (client, org) = self.reader()?;
        let row = client
            .query_opt(
                "SELECT store, ref, version FROM registry.read_connection_secret($1,$2,$3,$4)",
                &[&org, &project, &environment, credential],
            )
            .await
            .context("read the credential reference")?;
        match row {
            Some(row) => self.material(&row).await,
            None => Ok(None),
        }
    }

    /// The map from integration name to connection id of the environment.
    pub(crate) async fn env_connections(
        &self,
        project: &str,
        environment: &str,
    ) -> anyhow::Result<std::collections::BTreeMap<String, uuid::Uuid>> {
        let (client, org) = self.reader()?;
        Ok(client
            .query(
                "SELECT name, id FROM registry.read_env_connections($1,$2,$3)",
                &[&org, &project, &environment],
            )
            .await
            .context("read the environment connections")?
            .into_iter()
            .map(|row| (row.get(0), row.get(1)))
            .collect())
    }

    /// One published version of an integration type, or its newest one.
    pub(crate) async fn integration_version(
        &self,
        integration_type: &str,
        version: Option<i32>,
    ) -> anyhow::Result<Option<IntegrationVersion>> {
        let (client, _) = self.reader()?;
        let row = client
            .query_opt(
                "SELECT version, digest FROM registry.read_integration_version($1,$2)",
                &[&integration_type, &version],
            )
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
    pub principal_id: String,
    pub parameters: serde_json::Value,
}

/// One published version of an integration type.
#[derive(Clone, Debug)]
pub struct IntegrationVersion {
    pub version: i32,
    pub digest: Option<String>,
}

#[cfg(test)]
pub(crate) mod test_util;

#[cfg(test)]
mod tests {
    use super::*;
    use test_util::Fixture;

    #[tokio::test]
    async fn active_references_are_scoped_and_disabled_connections_refuse() {
        let _lock = wamn_test_postgres::lock();
        let mut fixture = Fixture::new().await;
        fixture.put("erp", "first", &["project-a"]).await;
        fixture.put("erp", "other", &["project-b"]).await;
        fixture.put("erp", "current", &["project-a"]).await;
        let reader = fixture.reader("project-a").await;
        assert_eq!(
            reader
                .read_connection("project-a", "org:acme:erp@2")
                .await
                .unwrap()
                .as_deref(),
            Some("current")
        );
        assert!(
            reader
                .read_connection("project-b", "org:acme:erp")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            reader
                .read_connection("project-a", "org:other:erp")
                .await
                .is_err()
        );
        assert!(
            reader
                .read_connection("project-a", "project:project-a:erp")
                .await
                .is_err()
        );
        let row = fixture.admin.query_one("SELECT id::text,service_principal_id::text FROM registry.connection WHERE project='project-a'", &[]).await.unwrap();
        let id: String = row.get(0);
        let principal: String = row.get(1);
        assert_eq!(
            reader
                .principal_for_connection("project-a", id.parse().unwrap())
                .await
                .unwrap()
                .as_deref(),
            Some(principal.as_str())
        );
        assert!(
            reader
                .database
                .as_ref()
                .unwrap()
                .simple_query("SELECT * FROM identity.credential")
                .await
                .is_err()
        );
        fixture.admin.batch_execute("SET ROLE wamn_system; UPDATE registry.integration SET status='disabled' WHERE project='project-a'; RESET ROLE").await.unwrap();
        assert!(
            reader
                .read_connection("project-a", "org:acme:erp")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            reader
                .principal_for_connection("project-a", id.parse().unwrap())
                .await
                .unwrap()
                .is_none()
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
        fixture.admin.batch_execute("SET ROLE wamn_system; UPDATE registry.integration SET status='disabled' WHERE project='project-a' AND name='files'; RESET ROLE").await.unwrap();
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
    async fn a_generation_carries_its_authentication_and_authorization_code_refuses() {
        use serde_json::json;
        use wamn_platform_identity::{auth_changes::AuthDrops, connections::administration};

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
        let http_only: bool = fixture
            .admin
            .query_one(
                "SELECT http_only FROM registry.auth_type WHERE code='hmac'",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        assert!(http_only);

        let tx = fixture.admin.transaction().await.unwrap();
        tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
        .await
        .unwrap();
        tx.batch_execute("SET LOCAL ROLE wamn_control")
            .await
            .unwrap();
        administration::execute(&tx, "acme", "integration.write", &json!({"project":"project-a","name":"interactive","integration_type":"http","auth_type":"oauth2_authorization_code"}).to_string(), &mut AuthDrops::default(), &mut wamn_platform_identity::connection_changes::ConnectionDrops::default()).await.unwrap();
        let error = administration::execute(
            &tx,
            "acme",
            "connection.write",
            &json!({"project":"project-a","env":"dev","name":"interactive","parameters":{}})
                .to_string(),
            &mut AuthDrops::default(),
            &mut wamn_platform_identity::connection_changes::ConnectionDrops::default(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, "unsupported_auth_type");
        assert_eq!(error.field, "auth_type");
    }

    #[tokio::test]
    async fn an_absent_reader_is_unavailable() {
        assert!(
            ConnectionCredentials::empty()
                .read_connection("project-a", "org:acme:erp")
                .await
                .is_err()
        );
    }
}
