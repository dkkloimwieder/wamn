//! Project login Secrets belong to the host plugin; pools start at workload bind.
use crate::{
    AuthorityClass, ClassCredentials, CredentialProvider, ProjectConfig, ResolvedCredential,
    StaticCredentialProvider, WamnPostgres, WamnPostgresConfig,
};
use anyhow::Context as _;
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HostProject {
    host: String,
    port: u16,
    database: String,
    logins: BTreeMap<String, String>,
    guest_pool_max_size: Option<usize>,
    wait_timeout_ms: Option<u64>,
    statement_timeout_ms: Option<u32>,
}

#[derive(Default)]
pub(super) struct HostCredentials(pub(super) RwLock<StaticCredentialProvider>);
impl CredentialProvider for HostCredentials {
    fn resolve(
        &self,
        project: &wamn_registry::ProjectId,
        class: AuthorityClass,
        key: Option<&wamn_registry::ProjectEnvKey>,
    ) -> anyhow::Result<Option<ResolvedCredential>> {
        self.0
            .read()
            .map_err(|_| anyhow::anyhow!("host credentials lock poisoned"))?
            .resolve(project, class, key)
    }
}

impl WamnPostgres {
    /// Parse host-owned project locations without reading credentials or building pools.
    pub fn from_host_config(config: &HashMap<String, String>) -> anyhow::Result<Self> {
        let mut projects = BTreeMap::new();
        for (project, config) in config {
            let project: wamn_registry::ProjectId = project.parse()?;
            let config: HostProject =
                serde_json::from_str(config).context("decode host project configuration")?;
            anyhow::ensure!(!config.logins.is_empty(), "project has no database logins");
            for (class, secret) in &config.logins {
                anyhow::ensure!(
                    AuthorityClass::ALL
                        .iter()
                        .any(|entry| entry.as_str() == class),
                    "unknown database authority class"
                );
                anyhow::ensure!(
                    !secret.is_empty()
                        && secret.bytes().all(|byte| byte.is_ascii_lowercase()
                            || byte.is_ascii_digit()
                            || byte == b'-'),
                    "invalid database Secret name"
                );
            }
            projects.insert(project, config);
        }
        let credentials = Arc::new(HostCredentials::default());
        let mut postgres =
            Self::with_provider(Arc::clone(&credentials) as Arc<dyn CredentialProvider>);
        postgres.host_projects = projects;
        postgres.host_credentials = Some(credentials);
        Ok(postgres)
    }

    pub(super) async fn bind_host_project(
        &self,
        project: &wamn_registry::ProjectId,
        key: &wamn_registry::ProjectEnvKey,
    ) -> anyhow::Result<()> {
        let Some(credentials) = &self.host_credentials else {
            return Ok(());
        };
        let _bind = self.host_project_bind.lock().await;
        let config = self
            .host_projects
            .get(project)
            .context("host has no logins for project")?;
        let mut urls = ClassCredentials::default();
        for class in AuthorityClass::ALL {
            let Some(secret) = config.logins.get(class.as_str()) else {
                continue;
            };
            let directory = std::path::Path::new("/etc/wasmcloud/host-plugin-secrets").join(secret);
            let username = tokio::fs::read_to_string(directory.join("username"))
                .await
                .context("read database username Secret")?;
            let password = tokio::fs::read_to_string(directory.join("password"))
                .await
                .context("read database password Secret")?;
            let mut url = url::Url::parse("postgres://localhost")?;
            url.set_host(Some(&config.host))
                .map_err(|_| anyhow::anyhow!("invalid project database host"))?;
            url.set_port(Some(config.port))
                .map_err(|()| anyhow::anyhow!("invalid project database port"))?;
            url.set_path(&config.database);
            url.set_username(&username)
                .map_err(|()| anyhow::anyhow!("invalid database username"))?;
            url.set_password(Some(&password))
                .map_err(|()| anyhow::anyhow!("invalid database password"))?;
            urls = urls.with_class(class, String::from(url));
        }
        let base = WamnPostgresConfig::from_env();
        let next = ProjectConfig {
            credentials: urls,
            guest_pool_max_size: config
                .guest_pool_max_size
                .unwrap_or(base.guest_pool_max_size),
            platform_pool_max_size: base.platform_pool_max_size,
            wait_timeout_ms: super::pool::bounded_wait_timeout_ms(
                config.wait_timeout_ms.unwrap_or(base.wait_timeout_ms),
            ),
            statement_timeout_ms: super::pool::bounded_statement_timeout_ms(u64::from(
                config
                    .statement_timeout_ms
                    .unwrap_or(base.statement_timeout_ms),
            )),
        };
        let changed = credentials
            .0
            .read()
            .map_err(|_| anyhow::anyhow!("host credentials lock poisoned"))?
            .project_credentials(project)
            .is_some_and(|previous| previous != &next.credentials);
        if changed {
            let retired: Vec<_> = {
                let mut pools = self.pools.write().expect("pools lock poisoned");
                let keys: Vec<_> = pools
                    .keys()
                    .filter(|key| key.project == *project)
                    .cloned()
                    .collect();
                keys.into_iter()
                    .filter_map(|key| pools.remove(&key))
                    .collect()
            };
            for pool in retired {
                pool.pool.close().await;
            }
        }
        credentials
            .0
            .write()
            .map_err(|_| anyhow::anyhow!("host credentials lock poisoned"))?
            .insert(project.clone(), next);
        for class in AuthorityClass::ALL {
            if config.logins.contains_key(class.as_str()) {
                self.ensure_pool(class, project, Some(key))
                    .await
                    .map_err(|_| anyhow::anyhow!("build project login pool"))?;
            }
        }
        Ok(())
    }
}
