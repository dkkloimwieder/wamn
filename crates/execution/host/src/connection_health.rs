//! Candidate health without changes to connection facts or credentials.

use std::sync::Arc;

use moka::sync::Cache;
use oci_client::secrets::RegistryAuth;
use wamn_engine::integration::{Integration, IntegrationBinding};
use wamn_package_artifact::registry_credentials::RegistryCredentialSource;
use wamn_package_artifact::{PackageRegistry, Repository};

use wamn_http_capability::{self, transport::HttpTransport};

/// Process-owned artifact cache. Candidate instances have fresh stores.
pub struct ConnectionHealth {
    transport: Arc<HttpTransport>,
    registry: Option<(PackageRegistry, RegistryCredentialSource)>,
    bytes: Cache<String, Arc<Vec<u8>>>,
}

impl std::fmt::Debug for ConnectionHealth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionHealth").finish_non_exhaustive()
    }
}

impl ConnectionHealth {
    pub fn new(transport: Arc<HttpTransport>) -> Self {
        Self {
            transport,
            registry: None,
            bytes: wamn_connections::connection_facts::bounded(),
        }
    }

    pub fn with_registry(
        mut self,
        registry: PackageRegistry,
        credentials: RegistryCredentialSource,
    ) -> Self {
        self.registry = Some((registry, credentials));
        self
    }

    /// Link candidate values and return only non-secret health diagnostics.
    pub async fn probe(
        &self,
        integration_type: &str,
        runtime: &str,
        digest: Option<&str>,
        parameters: &serde_json::Value,
        auth_type: &str,
        credential: Option<&str>,
    ) -> Result<(), String> {
        match runtime {
            "host" => match integration_type {
                "http" => {
                    wamn_http_capability::health(&self.transport, parameters, auth_type, credential)
                        .await
                        .map_err(|error| {
                            if matches!(error.parse::<u16>(), Ok(401 | 403 | 500..=599)) {
                                error
                            } else {
                                "HTTP health failed".to_owned()
                            }
                        })
                }
                "gcs" | "s3" => {
                    wamn_blobstore::plugin::health(integration_type, parameters, credential)
                        .await
                        .map_err(|_| "storage health failed".to_owned())
                }
                _ => Err("integration health unavailable".into()),
            },
            "wasm" | "native" => self
                .guest(integration_type, digest, parameters, auth_type, credential)
                .await
                .map_err(|_| "integration health failed".to_owned()),
            _ => Err("integration health unavailable".into()),
        }
    }

    async fn guest(
        &self,
        code: &str,
        digest: Option<&str>,
        parameters: &serde_json::Value,
        auth_type: &str,
        credential: Option<&str>,
    ) -> anyhow::Result<()> {
        use anyhow::Context as _;
        let digest = digest.context("integration digest absent")?;
        let bytes = if let Some(bytes) = self.bytes.get(digest) {
            bytes
        } else {
            let (registry, credentials) = self
                .registry
                .as_ref()
                .context("integration registry absent")?;
            let base = wamn_catalog::parse_component_artifact_base(&registry.artifact_base)?;
            let credential = credentials.credentials(base.registry()).await?;
            let repository = Repository::with_auth(
                registry,
                digest,
                RegistryAuth::Basic(credential.username().into(), credential.password().into()),
            )?;
            let bytes = Arc::new(
                wamn_package_artifact::integration::pull_from(&repository, code, digest).await?,
            );
            self.bytes.insert(digest.into(), Arc::clone(&bytes));
            bytes
        };
        let binding = IntegrationBinding::new(
            parameters["authority"]
                .as_str()
                .context("integration authority absent")?,
            credential.map(str::to_owned),
        )?
        .with_parameters(parameters.clone(), auth_type);
        let integration = Integration::load(&bytes, binding).await?;
        integration.health().await?.map_err(anyhow::Error::msg)
    }
}
