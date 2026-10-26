//! Host-owned environment credential store.
//!
//! Portable effects resolve a credential handle selected by the admitted
//! connection generation. Secret material never enters the guest or run facts.
//!
//! - Ordinary handles use the host-owned project identity. Org handles use
//!   the trusted host org and never fall back to project credentials.
//! - The dedicated database reader accesses immutable credential generations.
//! - No configured source is `unavailable`; a configured source missing the
//!   selected handle is `not-found`.

use std::collections::HashMap;

use anyhow::Context as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CredentialError {
    NotFound,
    Unavailable,
}

/// Project and org credentials read by the host.
pub struct WamnCredentials {
    /// Whether ANY backing source was configured. Distinguishes `unavailable`
    /// (no source — retryable) from `not-found` (source present, name absent —
    /// config-shaped).
    has_source: bool,
    /// project → { credential name → secret material }.
    projects: HashMap<String, HashMap<String, String>>,
    orgs: HashMap<String, HashMap<String, String>>,
    org: Option<String>,
    database: Option<std::sync::Arc<tokio_postgres::Client>>,
}

impl std::fmt::Debug for WamnCredentials {
    /// Names the shape only. The map holds secret material, so no key or value
    /// of it reaches a log.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WamnCredentials")
            .field("has_source", &self.has_source)
            .field("projects", &self.projects.len())
            .finish_non_exhaustive()
    }
}

impl WamnCredentials {
    /// Read credentials through the dedicated system reader, scoped by the host org.
    pub fn from_database(client: std::sync::Arc<tokio_postgres::Client>, org: &str) -> Self {
        Self {
            database: Some(client),
            org: Some(org.to_owned()),
            has_source: true,
            ..Self::empty()
        }
    }

    /// Read the immutable generation selected by a connection handle.
    pub async fn read_connection(
        &self,
        project: &str,
        handle: &str,
    ) -> anyhow::Result<Option<String>> {
        let Some(client) = &self.database else {
            return Ok(self.lookup_connection(project, handle));
        };
        let org = self
            .org
            .as_deref()
            .context("credential reader has no org")?;
        if handle.starts_with("org:") {
            let (handle_org, name) =
                wamn_catalog::parse_org_credential_handle(handle).map_err(anyhow::Error::msg)?;
            anyhow::ensure!(handle_org == org, "credential org mismatch");
            let generation = handle
                .rsplit_once('@')
                .map(|(_, generation)| generation.parse::<i64>())
                .transpose()?;
            let row = client.query_opt(
                "SELECT k.secret FROM registry.connection c JOIN registry.connection_generation g ON g.connection_id = c.id JOIN identity.credential k ON k.id = g.secret_id WHERE c.org = $1 AND c.name = $2 AND c.status = 'active' AND ($3::bigint IS NULL OR g.generation = $3) ORDER BY g.generation LIMIT 1",
                &[&org, &name, &generation],
            ).await?;
            return Ok(row.map(|row| row.get(0)));
        }
        self.read(project, handle).await
    }

    /// Read an imported project credential. An org handle never falls back here.
    pub async fn read(&self, project: &str, handle: &str) -> anyhow::Result<Option<String>> {
        if handle.starts_with("org:") {
            return Ok(None);
        }
        let Some(client) = &self.database else {
            return Ok(self.lookup(project, handle));
        };
        let org = self
            .org
            .as_deref()
            .context("credential reader has no org")?;
        Ok(client.query_opt("SELECT secret FROM identity.credential WHERE org = $1 AND project = $2 AND handle = $3 AND connection_id IS NULL", &[&org, &project, &handle]).await?.map(|row| row.get(0)))
    }

    /// A vault with NO backing source: every lookup is unavailable.
    /// Gates and credential-less deployments use this.
    pub fn empty() -> Self {
        Self {
            has_source: false,
            projects: HashMap::new(),
            orgs: HashMap::new(),
            org: None,
            database: None,
        }
    }

    /// A vault over an explicit per-project map (tests / the gate harness).
    pub fn from_projects(projects: HashMap<String, HashMap<String, String>>) -> Self {
        Self {
            has_source: true,
            projects,
            orgs: HashMap::new(),
            org: None,
            database: None,
        }
    }

    /// Parse in-memory credential maps for a test harness.
    pub fn from_json(text: &str) -> anyhow::Result<Self> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Envelope {
            projects: HashMap<String, HashMap<String, String>>,
            orgs: HashMap<String, HashMap<String, String>>,
        }
        let envelope: Envelope = serde_json::from_str(text).context("invalid credentials file")?;
        Ok(Self {
            has_source: true,
            projects: envelope.projects,
            orgs: envelope.orgs,
            org: None,
            database: None,
        })
    }

    /// Bind the trusted host org before sharing this process-lifetime snapshot.
    pub fn with_org(mut self, org: &str) -> Self {
        self.org = Some(org.to_owned());
        self
    }

    /// Resolve an org handle only within the host org, without project fallback.
    pub fn lookup_connection(&self, project: &str, handle: &str) -> Option<String> {
        if handle.starts_with("org:") {
            let (org, name) = wamn_catalog::parse_org_credential_handle(handle).ok()?;
            if self.org.as_deref() != Some(org) {
                return None;
            }
            return self.orgs.get(org)?.get(name).cloned();
        }
        self.lookup(project, handle)
    }

    /// Resolve a `(project, name)` secret for the native effect adapter.
    /// `None` if the name is absent or no source is configured.
    pub fn lookup(&self, project: &str, name: &str) -> Option<String> {
        self.resolve(project, name).ok()
    }

    /// Resolve `name` within `project`.
    fn resolve(&self, project: &str, name: &str) -> Result<String, CredentialError> {
        if !self.has_source {
            return Err(CredentialError::Unavailable);
        }
        self.projects
            .get(project)
            .and_then(|creds| creds.get(name))
            .cloned()
            .ok_or(CredentialError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_project_vault() -> WamnCredentials {
        WamnCredentials::from_projects(HashMap::from([(
            "proj-a".to_string(),
            HashMap::from([("notify-token".to_string(), "s3cr3t".to_string())]),
        )]))
    }

    /// The unavailable/not-found split: no source at all is retryable
    /// infrastructure; a present source lacking the name is config-shaped.
    #[test]
    fn resolve_distinguishes_no_source_from_unknown_name() {
        let empty = WamnCredentials::empty();
        assert!(matches!(
            empty.resolve("proj-a", "notify-token"),
            Err(CredentialError::Unavailable)
        ));
        let vault = one_project_vault();
        assert_eq!(vault.resolve("proj-a", "notify-token").unwrap(), "s3cr3t");
        assert!(matches!(
            vault.resolve("proj-a", "unknown"),
            Err(CredentialError::NotFound)
        ));
    }

    /// PROJECT SCOPING is the host-enforced boundary: the same name in a
    /// different project resolves that project's secret (or nothing) — a
    /// component can never read across projects.
    #[test]
    fn resolution_is_project_scoped() {
        let vault = WamnCredentials::from_projects(HashMap::from([
            (
                "proj-a".to_string(),
                HashMap::from([("token".to_string(), "secret-a".to_string())]),
            ),
            (
                "proj-b".to_string(),
                HashMap::from([("token".to_string(), "secret-b".to_string())]),
            ),
        ]));
        assert_eq!(vault.resolve("proj-a", "token").unwrap(), "secret-a");
        assert_eq!(vault.resolve("proj-b", "token").unwrap(), "secret-b");
        assert!(matches!(
            vault.resolve("proj-c", "token"),
            Err(CredentialError::NotFound)
        ));
    }

    #[test]
    fn credentials_file_parses_project_and_org_maps() {
        let vault = WamnCredentials::from_json(
            r#"{"projects":{"default":{"notify-token":"tok-1","other":"tok-2"},"p2":{}},"orgs":{}}"#,
        ).unwrap();
        assert_eq!(
            vault.lookup("default", "notify-token").as_deref(),
            Some("tok-1")
        );
        assert_eq!(vault.lookup("default", "other").as_deref(), Some("tok-2"));
        for text in [
            "[]",
            r#"{"p":"flat"}"#,
            r#"{"projects":{"p":{"n":7}},"orgs":{}}"#,
        ] {
            assert!(WamnCredentials::from_json(text).is_err());
        }
    }

    #[test]
    fn in_memory_material_preserves_the_selected_generation() {
        let original = r#"{"projects":{"proj-a":{"token":"generation-1"}},"orgs":{}}"#;
        let vault = WamnCredentials::from_json(original).unwrap();
        let rotated = WamnCredentials::from_json(
            r#"{"projects":{"proj-a":{"token":"generation-2"}},"orgs":{}}"#,
        )
        .unwrap();
        assert_eq!(
            vault.lookup("proj-a", "token").as_deref(),
            Some("generation-1")
        );
        assert_eq!(
            rotated.lookup("proj-a", "token").as_deref(),
            Some("generation-2")
        );
    }
}

#[cfg(test)]
mod org_tests {
    use super::*;

    #[test]
    fn org_map_scopes_connections_and_keeps_route_lookup_project_only() {
        let text = r#"{"projects":{"p":{"token":"project","org:other:erp":"fallback","org:acme:missing":"fallback"}},"orgs":{"acme":{"erp":"shared"},"other":{"erp":"other-secret"}}}"#;
        let unscoped = WamnCredentials::from_json(text).unwrap();
        assert!(unscoped.lookup_connection("p", "org:acme:erp").is_none());
        let vault = unscoped.with_org("acme");
        assert_eq!(
            vault.lookup_connection("p", "org:acme:erp").as_deref(),
            Some("shared")
        );
        assert_eq!(
            vault
                .lookup_connection("another-project", "org:acme:erp")
                .as_deref(),
            Some("shared")
        );
        assert_eq!(
            vault.lookup_connection("p", "token").as_deref(),
            Some("project")
        );
        assert!(vault.lookup_connection("p", "org:other:erp").is_none());
        assert!(vault.lookup_connection("p", "org:acme:missing").is_none());
        assert!(vault.lookup_connection("p", "org:acme:erp:extra").is_none());
        assert!(vault.lookup("p", "org:acme:erp").is_none());
    }

    #[test]
    fn invalid_envelopes_and_missing_files_have_distinct_results() {
        for text in [
            r#"{"version":3,"projects":{},"orgs":{}}"#,
            r#"{"projects":{},"orgs":{"acme":{"erp":7}}}"#,
            r#"{"projects":{},"orgs":{},"extra":true}"#,
        ] {
            assert!(WamnCredentials::from_json(text).is_err());
        }
        assert!(
            WamnCredentials::empty()
                .lookup_connection("p", "org:acme:erp")
                .is_none()
        );
    }
}

#[cfg(test)]
mod database_tests {
    use super::*;
    use wamn_platform_identity::auth_changes::{AuthChanges, AuthDrops, auth_bucket, identity_key};
    use wamn_platform_identity::connections::{
        ConnectionWrite, import_credentials, rotate, write_connection,
    };

    #[tokio::test]
    async fn live_import_rotation_and_principal_drops_use_the_store() {
        let _lock = wamn_test_postgres::lock();
        let database = wamn_control_provision::test_database::system();
        let (mut client, driver) = tokio_postgres::connect(database.url(), tokio_postgres::NoTls)
            .await
            .unwrap();
        tokio::spawn(async move {
            driver.await.unwrap();
        });
        client.batch_execute("SET ROLE wamn_system; INSERT INTO registry.orgs(id, placement_type) VALUES ('acme','dedicated'); INSERT INTO registry.projects(org,id) VALUES ('acme','project')").await.unwrap();
        let tx = client.transaction().await.unwrap();
        tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
        .await
        .unwrap();
        let mut drops = AuthDrops::default();
        let id = write_connection(&tx, "acme", &ConnectionWrite {
            id: None, integration: "http".into(), external_system_id: None, name: "erp".into(), transport: "http".into(), exchange: "request".into(),
            parameters: serde_json::json!({"primary-authority":"https://example.invalid","tls-verification":"verify-authority"}), status: "active".into(),
        }, &mut drops).await.unwrap();
        let principal: String = tx.query_one("SELECT service_principal_id::text FROM registry.connection WHERE id=$1::text::uuid", &[&id]).await.unwrap().get(0);
        import_credentials(&tx, "acme", r#"{"orgs":{"acme":{"erp":"first"}},"projects":{"project":{"legacy":"project-secret"}}}"#).await.unwrap();
        rotate(&tx, "acme", &id, "second").await.unwrap();
        tx.commit().await.unwrap();
        let (_server, nats) = crate::auth_cache::test_util::nats().await;
        let bucket = auth_bucket(&nats).await.unwrap();
        let key = identity_key(&principal);
        bucket.put(&key, "cached".into()).await.unwrap();
        AuthChanges::new(nats).publish(drops).await;
        assert!(bucket.get(&key).await.unwrap().is_none());
        client.batch_execute("RESET ROLE").await.unwrap();
        client
            .batch_execute(&wamn_control_provision::sql::grant_connection_reader_surface_sql())
            .await
            .unwrap();
        client
            .batch_execute("SET ROLE wamn_conn_reader")
            .await
            .unwrap();
        let vault = WamnCredentials::from_database(std::sync::Arc::new(client), "acme");
        assert_eq!(
            vault
                .read_connection("project", "org:acme:erp@2")
                .await
                .unwrap()
                .as_deref(),
            Some("first")
        );
        assert_eq!(
            vault
                .read_connection("project", "org:acme:erp@3")
                .await
                .unwrap()
                .as_deref(),
            Some("second")
        );
        assert_eq!(
            vault
                .read_connection("project", "org:acme:erp")
                .await
                .unwrap()
                .as_deref(),
            Some("first")
        );
        assert_eq!(
            vault.read("project", "legacy").await.unwrap().as_deref(),
            Some("project-secret")
        );
        assert!(
            vault
                .read_connection("project", "org:other:erp@2")
                .await
                .is_err()
        );
        assert!(
            vault
                .read_connection("project", "org:acme:missing@2")
                .await
                .unwrap()
                .is_none()
        );
        assert!(vault.read("other", "legacy").await.unwrap().is_none());
    }
}
