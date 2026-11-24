//! Package artifact reads for deployment.

use std::path::PathBuf;

use wamn_oci::package_artifact::{PackageRegistry, registry_credentials::RegistryCredentialSource};

/// Registry connection inputs supplied by the operation worker.
#[derive(Debug)]
pub struct DeployPackages {
    pub registry_token_metadata: bool,
    pub registry_auth_file: PathBuf,
    pub insecure_registry: bool,
    pub oci_ca_paths: Vec<PathBuf>,
}

impl DeployPackages {
    pub async fn repository(&self, base: &str) -> anyhow::Result<PackageRegistry> {
        let credentials = if self.registry_token_metadata {
            let base = wamn_oci_reference::parse_component_artifact_base(base)?;
            Some(
                RegistryCredentialSource::MetadataServer
                    .credentials(base.registry())
                    .await?,
            )
        } else {
            None
        };
        Ok(PackageRegistry {
            artifact_base: base.to_owned(),
            credentials,
            registry_auth_file: self.registry_auth_file.clone(),
            insecure_registry: self.insecure_registry,
            oci_ca_paths: self.oci_ca_paths.clone(),
        })
    }
}
