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
            "SELECT store, ref, version, principal_id, external_system FROM registry.read_connection_credential($1,$2,$3)",
            &[&org, &project, &name],
        ).await.context("read connection credential authority")
    }

    /// Read one active host store through the login's environment scope.
    pub(crate) async fn file_configuration(
        &self,
        project: &str,
        environment: &str,
        id: &str,
    ) -> anyhow::Result<Option<(i64, serde_json::Value)>> {
        let client = self
            .database
            .as_ref()
            .context("connection reader unavailable")?;
        let org = self
            .org
            .as_deref()
            .context("connection reader has no org")?;
        Ok(client.query_opt(
            "SELECT generation,parameters FROM registry.read_file_connection($1,$2,$3,$4::text::uuid)",
            &[&org, &project, &environment, &id],
        ).await?.map(|row| (row.get(0), row.get(1))))
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

    /// Refuse disabled connections and integrations outside the reader's environment.
    pub async fn connection_enabled(&self, project: &str, name: &str) -> anyhow::Result<bool> {
        Ok(self.connection(project, name).await?.is_some())
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

    /// Read the integration's external system identity under the same login scope.
    pub(crate) async fn external_system_id(
        &self,
        project: &str,
        handle: &str,
    ) -> anyhow::Result<Option<String>> {
        let (name, _) = self.handle(handle)?;
        Ok(self
            .connection(project, name)
            .await?
            .and_then(|row| row.get(4)))
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

    /// Read a binding mode from the registry under the reader login's scope.
    pub async fn binding_read_only(
        &self,
        project: &str,
        environment: &str,
        application: &str,
        alias: &str,
    ) -> anyhow::Result<bool> {
        let client = self
            .database
            .as_ref()
            .context("connection credentials unavailable")?;
        let org = self
            .org
            .as_deref()
            .context("credential reader has no org")?;
        let mode: Option<String> = client
            .query_one(
                "SELECT registry.lookup_connection_binding_mode($1,$2,$3,$4,$5)",
                &[&org, &project, &environment, &application, &alias],
            )
            .await?
            .get(0);
        match mode.as_deref() {
            Some("read") => Ok(true),
            Some("write") => Ok(false),
            _ => anyhow::bail!("connection binding mode unavailable"),
        }
    }
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
    async fn file_reads_select_the_active_generation_and_refuse_other_types() {
        let _lock = wamn_test_postgres::lock();
        let mut fixture = Fixture::new().await;
        fixture.put_store("files", "first", &["project-a"]).await;
        fixture.put("erp", "http", &["project-a"]).await;
        let reader = fixture.reader("project-a").await;
        let rows = fixture.admin.query("SELECT c.id::text,i.name FROM registry.connection c JOIN registry.integration i ON i.id=c.integration_id WHERE c.project='project-a'", &[]).await.unwrap();
        let store: String = rows
            .iter()
            .find(|row| row.get::<_, String>(1) == "files")
            .unwrap()
            .get(0);
        let http: String = rows
            .iter()
            .find(|row| row.get::<_, String>(1) == "erp")
            .unwrap()
            .get(0);
        let first = reader
            .file_configuration("project-a", "dev", &store)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.1["provider"], "s3");
        fixture.put_store("files", "current", &["project-a"]).await;
        let current = reader
            .file_configuration("project-a", "dev", &store)
            .await
            .unwrap()
            .unwrap();
        assert!(current.0 > first.0);
        assert!(
            reader
                .file_configuration("project-a", "dev", &http)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            reader
                .file_configuration("project-b", "dev", &store)
                .await
                .unwrap()
                .is_none()
        );
        fixture.admin.batch_execute("SET ROLE wamn_system; UPDATE registry.integration SET status='disabled' WHERE project='project-a' AND name='files'; RESET ROLE").await.unwrap();
        assert!(
            reader
                .file_configuration("project-a", "dev", &store)
                .await
                .unwrap()
                .is_none()
        );
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
