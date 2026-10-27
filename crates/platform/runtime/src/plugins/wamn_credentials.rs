//! Host-owned connection credentials read through login-scoped database functions.
//!
//! Secret material never enters guests or stored run facts. An absent reader
//! is unavailable. A reader with no matching enabled connection returns none.

use std::sync::Arc;

use anyhow::Context as _;

/// Connection credentials scoped by the authenticated reader login.
#[derive(Clone)]
pub struct WamnCredentials {
    org: Option<String>,
    database: Option<Arc<tokio_postgres::Client>>,
}

impl std::fmt::Debug for WamnCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WamnCredentials")
            .field("available", &self.database.is_some())
            .finish_non_exhaustive()
    }
}

impl WamnCredentials {
    /// Use a dedicated system reader whose login fixes its org and project.
    pub fn from_database(client: Arc<tokio_postgres::Client>, org: &str) -> Self {
        Self {
            org: Some(org.to_owned()),
            database: Some(client),
        }
    }

    /// Keep the explicit unavailable state for hosts without a credential reader.
    pub fn empty() -> Self {
        Self {
            org: None,
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
        generation: Option<i64>,
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
            "SELECT secret, principal_id, external_system FROM registry.lookup_connection_credential($1,$2,$3,$4)",
            &[&org, &project, &name, &generation],
        ).await.context("read connection credential authority")
    }

    /// Read the selected generation, or the current generation of an unpinned handle.
    pub async fn read_connection(
        &self,
        project: &str,
        handle: &str,
    ) -> anyhow::Result<Option<String>> {
        let (name, generation) = self.handle(handle)?;
        Ok(self
            .connection(project, name, generation)
            .await?
            .and_then(|row| row.get(0)))
    }

    /// Refuse disabled connections and connections that this project does not enable.
    pub async fn connection_enabled(&self, project: &str, name: &str) -> anyhow::Result<bool> {
        Ok(self.connection(project, name, None).await?.is_some())
    }

    /// Resolve a connection principal through the same authority as its credential.
    pub async fn inbound_principal(
        &self,
        project: &str,
        handle: &str,
    ) -> anyhow::Result<Option<String>> {
        let (name, generation) = self.handle(handle)?;
        Ok(self
            .connection(project, name, generation)
            .await?
            .and_then(|row| row.get(1)))
    }

    /// Read the projected external system id through the same login-scoped authority.
    pub(crate) async fn external_system_id(
        &self,
        project: &str,
        handle: &str,
    ) -> anyhow::Result<Option<String>> {
        let (name, generation) = self.handle(handle)?;
        Ok(self
            .connection(project, name, generation)
            .await?
            .and_then(|row| row.get(2)))
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
pub(crate) mod test_util {
    use super::*;
    use wamn_platform_identity::{
        auth_changes::AuthDrops,
        connections::{self, ConnectionWrite},
    };

    pub(crate) struct Fixture {
        pub database: wamn_test_postgres::Database,
        pub admin: tokio_postgres::Client,
    }

    impl Fixture {
        pub async fn new() -> Self {
            let database = wamn_control_provision::test_database::system();
            let (admin, driver) = tokio_postgres::connect(database.url(), tokio_postgres::NoTls)
                .await
                .unwrap();
            tokio::spawn(driver);
            admin.batch_execute("SET ROLE wamn_system").await.unwrap();
            for project in ["project-a", "project-b", "host-project", "other-project"] {
                admin
                    .batch_execute(
                        &wamn_control_provision::test_database::seed_org_project_env_sql(
                            "acme", project, "dev",
                        ),
                    )
                    .await
                    .unwrap();
            }
            admin.batch_execute("RESET ROLE").await.unwrap();
            Self { database, admin }
        }

        pub async fn put(&mut self, name: &str, secret: &str, projects: &[&str]) {
            let tx = self.admin.transaction().await.unwrap();
            tx.batch_execute("SET LOCAL ROLE wamn_control")
                .await
                .unwrap();
            tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
                wamn_control_provision::PlatformComponent::Provisioning,
            ))
            .await
            .unwrap();
            connections::write_connection(&tx, "acme", &ConnectionWrite {
                name: name.into(), integration: "http".into(), external_system: None,
                transport: "http".into(), exchange: "request".into(), status: "active".into(),
                parameters: serde_json::json!({"primary-authority":"https://example.invalid","tls-verification":"verify-authority"}),
            }, &mut AuthDrops::default()).await.unwrap();
            for project in projects {
                connections::write(
                    &tx,
                    "acme",
                    "project_connection.enable",
                    &serde_json::json!({"project":project,"connection":name}).to_string(),
                    &mut AuthDrops::default(),
                )
                .await
                .unwrap();
            }
            connections::rotate(&tx, "acme", name, secret)
                .await
                .unwrap();
            tx.commit().await.unwrap();
        }

        pub async fn reader(&self, project: &str) -> WamnCredentials {
            let role = format!("c35_{}_{}", self.database.name(), project.replace('-', "_"));
            self.admin
                .batch_execute(&format!(
                    "BEGIN; {} {}",
                    wamn_control_provision::bind_platform_principal_sql(
                        wamn_control_provision::PlatformComponent::Provisioning,
                    ),
                    wamn_control_provision::sql::prepare_workload_generation_sql(
                        wamn_control_provision::WorkloadRoleFamily::ConnectionReader,
                        self.database.name(),
                        &role,
                        "fixture-password",
                        "2099-01-01T00:00:00Z",
                    ),
                ))
                .await
                .unwrap();
            self.admin.batch_execute("GRANT EXECUTE ON FUNCTION registry.lookup_connection_binding_mode(text,text,text,text,text) TO wamn_conn_reader").await.unwrap();
            self.admin
                .execute(
                    wamn_control_provision::sql::upsert_control_author_tenant_mapping_sql(),
                    &[
                        &role,
                        &format!("acme--{project}--dev"),
                        &"acme",
                        &project,
                        &"dev",
                    ],
                )
                .await
                .unwrap();
            self.admin.batch_execute("COMMIT").await.unwrap();
            let mut url = url::Url::parse(self.database.url()).unwrap();
            url.set_username(&role).unwrap();
            url.set_password(Some("fixture-password")).unwrap();
            let (client, driver) = tokio_postgres::connect(url.as_str(), tokio_postgres::NoTls)
                .await
                .unwrap();
            tokio::spawn(driver);
            WamnCredentials::from_database(Arc::new(client), "acme")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_util::Fixture;

    #[tokio::test]
    async fn resolve_distinguishes_no_source_from_unknown_name() {
        let _lock = wamn_test_postgres::lock();
        let fixture = Fixture::new().await;
        let vault = fixture.reader("project-a").await;
        assert!(
            vault
                .read_connection("project-a", "org:acme:unknown")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            WamnCredentials::empty()
                .read_connection("project-a", "org:acme:token")
                .await
                .unwrap_err()
                .to_string(),
            "connection credentials unavailable"
        );
    }

    #[tokio::test]
    async fn resolution_is_project_scoped() {
        let _lock = wamn_test_postgres::lock();
        let mut fixture = Fixture::new().await;
        fixture.put("token-a", "secret-a", &["project-a"]).await;
        fixture.put("token-b", "secret-b", &["project-b"]).await;
        let a = fixture.reader("project-a").await;
        let b = fixture.reader("project-b").await;
        assert_eq!(
            a.read_connection("project-a", "org:acme:token-a")
                .await
                .unwrap()
                .as_deref(),
            Some("secret-a")
        );
        assert_eq!(
            b.read_connection("project-b", "org:acme:token-b")
                .await
                .unwrap()
                .as_deref(),
            Some("secret-b")
        );
        assert!(
            a.read_connection("project-a", "org:acme:token-b")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            a.read_connection("project-b", "org:acme:token-b")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            a.read_connection("project-c", "org:acme:token-a")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn invalid_handles_and_missing_sources_have_distinct_results() {
        let _lock = wamn_test_postgres::lock();
        let fixture = Fixture::new().await;
        let vault = fixture.reader("project-a").await;
        for handle in [
            "token",
            "org:acme:erp:extra",
            "org:acme:erp@no",
            "org:other:erp",
        ] {
            assert!(vault.read_connection("project-a", handle).await.is_err());
        }
        assert!(
            vault
                .read_connection("project-a", "org:acme:missing")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            WamnCredentials::empty()
                .read_connection("project-a", "org:acme:missing")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn credential_import_refuses_invalid_envelopes_and_unknown_connections() {
        let _lock = wamn_test_postgres::lock();
        let mut fixture = Fixture::new().await;
        fixture.put("erp", "secret", &["project-a"]).await;
        for text in [
            "[]",
            r#"{"p":"flat"}"#,
            r#"{"projects":{"project-a":{"erp":7}},"orgs":{}}"#,
            r#"{"version":3,"projects":{},"orgs":{}}"#,
            r#"{"projects":{},"orgs":{"acme":{"erp":7}}}"#,
            r#"{"projects":{},"orgs":{},"extra":true}"#,
            r#"{"projects":{"project-a":{"missing":"secret"}},"orgs":{}}"#,
        ] {
            let tx = fixture.admin.transaction().await.unwrap();
            tx.batch_execute("SET LOCAL ROLE wamn_control")
                .await
                .unwrap();
            assert!(
                wamn_platform_identity::connections::import_credentials(&tx, "acme", text)
                    .await
                    .is_err(),
                "{text}"
            );
            tx.rollback().await.unwrap();
        }
    }

    #[tokio::test]
    async fn no_source_is_unavailable() {
        assert_eq!(
            WamnCredentials::empty()
                .read_connection("project-a", "org:acme:token")
                .await
                .unwrap_err()
                .to_string(),
            "connection credentials unavailable"
        );
        assert!(
            WamnCredentials::empty()
                .connection_enabled("project-a", "erp")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn binding_modes_are_login_scoped_and_missing_rows_refuse() {
        let _lock = wamn_test_postgres::lock();
        let mut fixture = Fixture::new().await;
        fixture.put("erp", "secret", &["project-a"]).await;
        fixture.admin.batch_execute("SET ROLE wamn_system; INSERT INTO registry.environment_connection_binding(org,project,env,application,alias,target_id,connection_id,generation,mode,exclusive) SELECT 'acme','project-a','dev','app','erp',t.id,c.id,c.current_generation,'read',false FROM registry.target t,registry.connection c WHERE t.name='cloud' AND c.org='acme' AND c.name='erp'; RESET ROLE").await.unwrap();
        let vault = fixture.reader("project-a").await;
        assert!(
            vault
                .binding_read_only("project-a", "dev", "app", "erp")
                .await
                .unwrap()
        );
        for (project, environment, application, alias) in [
            ("project-b", "dev", "app", "erp"),
            ("project-a", "prod", "app", "erp"),
            ("project-a", "dev", "other", "erp"),
            ("project-a", "dev", "app", "missing"),
        ] {
            assert!(
                vault
                    .binding_read_only(project, environment, application, alias)
                    .await
                    .is_err()
            );
        }
        assert!(
            vault
                .database
                .as_ref()
                .unwrap()
                .simple_query("SELECT * FROM registry.environment_connection_binding")
                .await
                .is_err()
        );
        let login: String = vault
            .database
            .as_ref()
            .unwrap()
            .query_one("SELECT session_user::text", &[])
            .await
            .unwrap()
            .get(0);
        fixture
            .admin
            .execute(
                "DELETE FROM wamn_authority.author_login_tenants WHERE login_identity=$1",
                &[&login],
            )
            .await
            .unwrap();
        assert!(
            vault
                .read_connection("project-a", "org:acme:erp")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            vault
                .binding_read_only("project-a", "dev", "app", "erp")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn live_import_rotation_and_principal_drops_use_the_store() {
        use wamn_platform_identity::auth_changes::{
            AuthChanges, AuthDrops, auth_bucket, identity_key,
        };
        use wamn_platform_identity::connections;
        let _lock = wamn_test_postgres::lock();
        let mut fixture = Fixture::new().await;
        fixture.put("erp", "first", &["project-a"]).await;
        fixture
            .put("project-secret", "original", &["project-a"])
            .await;
        let tx = fixture.admin.transaction().await.unwrap();
        tx.batch_execute("SET LOCAL ROLE wamn_control")
            .await
            .unwrap();
        connections::import_credentials(
            &tx,
            "acme",
            r#"{"orgs":{},"projects":{"project-a":{"project-secret":{"connection":"project-secret","secret":"project-secret"}}}}"#,
        )
        .await
        .unwrap();
        let mut drops = AuthDrops::default();
        connections::write_connection(&tx, "acme", &connections::ConnectionWrite {
            name: "erp".into(), integration: "http".into(), external_system: None,
            transport: "http".into(), exchange: "request".into(), status: "active".into(),
            parameters: serde_json::json!({"primary-authority":"https://example.invalid","tls-verification":"verify-authority"}),
        }, &mut drops).await.unwrap();
        connections::rotate(&tx, "acme", "erp", "second")
            .await
            .unwrap();
        let principal: String = tx.query_one("SELECT service_principal_id::text FROM registry.connection_rows('acme') WHERE name='erp'", &[]).await.unwrap().get(0);
        tx.commit().await.unwrap();
        let (_server, nats) = crate::auth_cache::test_util::nats().await;
        let bucket = auth_bucket(&nats).await.unwrap();
        let key = identity_key(&principal);
        bucket.put(&key, "cached".into()).await.unwrap();
        AuthChanges::new(nats).publish(drops).await;
        assert!(bucket.get(&key).await.unwrap().is_none());
        let vault = fixture.reader("project-a").await;
        assert_eq!(
            vault
                .read_connection("project-a", "org:acme:project-secret")
                .await
                .unwrap()
                .as_deref(),
            Some("project-secret")
        );
        for (handle, expected) in [
            ("org:acme:erp@2", "first"),
            ("org:acme:erp@3", "second"),
            ("org:acme:erp", "second"),
        ] {
            assert_eq!(
                vault
                    .read_connection("project-a", handle)
                    .await
                    .unwrap()
                    .as_deref(),
                Some(expected)
            );
        }
        assert!(
            vault
                .read_connection("project-a", "org:other:erp@2")
                .await
                .is_err()
        );
        assert!(
            vault
                .read_connection("project-a", "org:acme:missing@2")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            vault
                .read_connection("other-project", "org:acme:erp@2")
                .await
                .unwrap()
                .is_none()
        );
        for relation in [
            "identity.credential",
            "registry.connection",
            "registry.connection_generation",
            "registry.project_connection",
        ] {
            assert_eq!(
                vault
                    .database
                    .as_ref()
                    .unwrap()
                    .simple_query(&format!("SELECT * FROM {relation}"))
                    .await
                    .unwrap_err()
                    .code(),
                Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE)
            );
        }
        let other = fixture.reader("project-b").await;
        assert!(
            other
                .read_connection("project-b", "org:acme:erp@2")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            vault
                .inbound_principal("project-a", "org:acme:erp")
                .await
                .unwrap()
                .as_deref(),
            Some(principal.as_str())
        );
        fixture.admin.batch_execute("SET ROLE wamn_control; SELECT registry.configure_connection('acme','project_connection.disable','{\"project\":\"project-a\",\"connection\":\"erp\"}'); RESET ROLE").await.unwrap();
        assert!(
            vault
                .read_connection("project-a", "org:acme:erp@2")
                .await
                .unwrap()
                .is_none()
        );
        assert!(!vault.connection_enabled("project-a", "erp").await.unwrap());
        assert!(
            vault
                .inbound_principal("project-a", "org:acme:erp")
                .await
                .unwrap()
                .is_none()
        );
    }
}

#[cfg(test)]
mod handle_tests {
    use super::*;

    #[test]
    fn credential_handles_use_the_reader_org_and_pin_the_generation() {
        let reader = WamnCredentials {
            org: Some("file-org".to_owned()),
            database: None,
        };
        assert_eq!(
            reader.connection_handle("files", 7).unwrap(),
            "org:file-org:files@7"
        );
        assert!(reader.connection_handle("files", 0).is_err());
        assert!(reader.connection_handle("files@2", 7).is_err());
        assert!(
            WamnCredentials::empty()
                .connection_handle("files", 7)
                .is_err()
        );
    }
}
