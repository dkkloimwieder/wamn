//! Project login configuration and its mounted credentials.
use crate::{
    WamnPostgres,
    pool::{
        ClassCredentials, CredentialProvider, ProjectConfig, ResolvedCredential,
        StaticCredentialProvider,
    },
};
use anyhow::Context as _;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use wamn_identity_protocol::AuthorityClass;

/// Public database coordinates and mounted login references for one host project.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostProject {
    host: String,
    port: u16,
    database: String,
    logins: BTreeMap<String, String>,
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
    /// Validate host-owned project locations without reading credentials or building pools.
    pub fn from_host_config(
        projects: BTreeMap<wamn_registry::ProjectId, HostProject>,
    ) -> anyhow::Result<Self> {
        for config in projects.values() {
            anyhow::ensure!(!config.logins.is_empty(), "project has no database logins");
            for (class, secret) in &config.logins {
                anyhow::ensure!(
                    AuthorityClass::ALL
                        .iter()
                        .any(|entry| entry.as_str() == class),
                    "unknown database authority class"
                );
                crate::credential_directory(std::path::Path::new(crate::DATABASE_DIR), secret)?;
            }
        }
        let credentials = Arc::new(HostCredentials::default());
        let mut postgres =
            Self::with_provider(Arc::clone(&credentials) as Arc<dyn CredentialProvider>);
        postgres.host_projects = projects;
        postgres.host_credentials = Some(credentials);
        Ok(postgres)
    }

    pub async fn bind_host_project(
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
            let directory =
                crate::credential_directory(std::path::Path::new(crate::DATABASE_DIR), secret)?;
            let username: wamn_identity_protocol::LoginName = match class {
                AuthorityClass::GuestSql => wamn_role::APP_LOGIN,
                AuthorityClass::DynamicSql => wamn_role::QUERY_LOGIN,
                AuthorityClass::Platform => wamn_role::PLATFORM_LOGIN,
            }
            .parse()?;
            let mut url = url::Url::parse("postgres://localhost")?;
            url.set_host(Some(&config.host))
                .map_err(|_| anyhow::anyhow!("invalid project database host"))?;
            url.set_port(Some(config.port))
                .map_err(|()| anyhow::anyhow!("invalid project database port"))?;
            url.set_path(&config.database);
            url.set_username(username.as_str())
                .map_err(|()| anyhow::anyhow!("invalid database username"))?;
            url.query_pairs_mut()
                .append_pair("sslrootcert", &directory.join("ca.crt").to_string_lossy());
            let credential_exists = async |path: &std::path::Path| {
                tokio::fs::try_exists(path)
                    .await
                    .with_context(|| format!("inspect database credential {}", path.display()))
            };
            let cert = directory.join("tls.crt");
            let key = directory.join("tls.key");
            let has_cert = credential_exists(&cert).await?;
            let has_key = credential_exists(&key).await?;
            if !has_cert && !has_key {
                let password = crate::read_password(&directory, &username).await?;
                url.set_password(Some(&password))
                    .map_err(|()| anyhow::anyhow!("invalid database password"))?;
            } else {
                anyhow::ensure!(
                    has_cert && has_key && !credential_exists(&directory.join("password")).await?,
                    "database credential {} requires tls.crt and tls.key together without a password file",
                    directory.display()
                );
                url.query_pairs_mut()
                    .append_pair("sslcert", &cert.to_string_lossy())
                    .append_pair("sslkey", &key.to_string_lossy());
            }
            urls = urls.with_class(class, String::from(url));
        }
        let next = ProjectConfig {
            credentials: urls,
            guest_pool_max_size: 16,
            platform_pool_max_size: 8,
            wait_timeout_ms: super::pool::bounded_wait_timeout_ms(2_000),
            statement_timeout_ms: super::pool::bounded_statement_timeout_ms(5_000),
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
