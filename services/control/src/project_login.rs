//! The `wamn_platform` login of each project environment, built at login from
//! `registry.project_env`. Control reads the login certificate Secret
//! and the server CA copy through the Kubernetes API and keeps the PEM bytes
//! in memory only.

use anyhow::Context as _;
use k8s_openapi::api::core::v1::{ConfigMap, Secret};
use kube::Api;
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use wamn_kubernetes::client::{KubernetesApi, REQUEST_TIMEOUT};
use wamn_registry::{EnvName, OrgId, ProjectId, Triple, instance_suffix::InstanceSuffix};

/// Control's reads of its project logins in its org.
#[derive(Debug)]
pub(crate) struct ProjectLogins {
    kubernetes: KubernetesApi,
    control: sqlx::PgPool,
    org: OrgId,
    #[cfg(test)]
    pub(crate) test_logins: std::collections::HashMap<Triple, PgConnectOptions>,
}

impl ProjectLogins {
    pub(crate) fn new(kubernetes: KubernetesApi, control: sqlx::PgPool, org: OrgId) -> Self {
        Self {
            kubernetes,
            control,
            org,
            #[cfg(test)]
            test_logins: std::collections::HashMap::new(),
        }
    }

    /// The login of one recorded project environment.
    pub(crate) async fn options(
        &self,
        project: &ProjectId,
        env: &EnvName,
    ) -> anyhow::Result<PgConnectOptions> {
        let triple = Triple::new(self.org.clone(), env.clone(), project.clone());
        let instance = wamn_env::registry::deployment::instance_suffix(
            &mut *self.control.acquire().await?,
            &triple,
        )
        .await?
        .with_context(|| format!("project deployment {triple} is absent"))?;
        self.login(&triple, &instance).await
    }

    /// The login of one project environment generation.
    pub(crate) async fn login(
        &self,
        triple: &Triple,
        instance: &InstanceSuffix,
    ) -> anyhow::Result<PgConnectOptions> {
        #[cfg(test)]
        if let Some(options) = self.test_logins.get(triple) {
            return Ok(options.clone());
        }
        let namespace = self.namespace(&triple.env).await?;
        self.login_in(triple, instance, &namespace).await
    }

    pub(crate) async fn namespace(&self, env: &EnvName) -> anyhow::Result<String> {
        let mut connection = self.control.acquire().await?;
        wamn_env::registry::deployment::namespace(&mut connection, &self.org, env)
            .await?
            .with_context(|| format!("env {}/{env} is not provisioned", self.org))
    }

    pub(crate) async fn oauth_key(
        &self,
        namespace: &str,
    ) -> anyhow::Result<wamn_envelope::encryption::Envelope> {
        let secrets = Api::<Secret>::namespaced(self.kubernetes.client(), namespace);
        let secret = tokio::time::timeout(
            REQUEST_TIMEOUT,
            secrets.get(wamn_platform_install::provision_org::OAUTH_SECRET),
        )
        .await??;
        let key = secret
            .data
            .and_then(|mut data| data.remove("key"))
            .context("environment OAuth Secret has no key")?;
        let bytes = zeroize::Zeroizing::new(key.0);
        Ok(wamn_key::from_secret(&bytes)?)
    }

    pub(crate) async fn login_in(
        &self,
        triple: &Triple,
        instance: &InstanceSuffix,
        namespace: &str,
    ) -> anyhow::Result<PgConnectOptions> {
        #[cfg(test)]
        if let Some(options) = self.test_logins.get(triple) {
            return Ok(options.clone());
        }
        let config = wamn_identity::platform_login::platform_connection(
            wamn_postgres::database::connection_for(triple, instance, namespace),
        )?;
        let server_ca = wamn_registry::name::project_server_ca_copy(
            &wamn_registry::name::project_cluster(&config.database),
        );
        let secrets = Api::<Secret>::namespaced(self.kubernetes.client(), namespace);
        let maps = Api::<ConfigMap>::namespaced(self.kubernetes.client(), namespace);
        let (login, ca) = tokio::time::timeout(REQUEST_TIMEOUT, async {
            tokio::try_join!(secrets.get(&config.credentials.name), maps.get(&server_ca))
        })
        .await
        .with_context(|| format!("read the login of {triple}"))?
        .with_context(|| format!("read the login of {triple}"))?;
        let mut login = login
            .data
            .with_context(|| format!("Secret {} has no data", config.credentials.name))?;
        let mut entry = |key: &str| {
            login
                .remove(key)
                .map(|bytes| bytes.0)
                .with_context(|| format!("Secret {} has no {key}", config.credentials.name))
        };
        let (certificate, key) = (entry("tls.crt")?, entry("tls.key")?);
        let ca = ca
            .data
            .and_then(|mut data| data.remove("ca.crt"))
            .with_context(|| format!("ConfigMap {server_ca} has no ca.crt"))?;
        Ok(PgConnectOptions::new_without_pgpass()
            .host(&config.host)
            .port(config.port)
            .database(&config.database)
            .username(config.username.as_str())
            .ssl_mode(PgSslMode::VerifyFull)
            .ssl_root_cert_from_pem(ca.into_bytes())
            .ssl_client_cert_from_pem(certificate)
            .ssl_client_key_from_pem(key))
    }
}
