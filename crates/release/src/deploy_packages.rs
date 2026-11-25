//! Registry reads for deployment.

use std::path::PathBuf;

use anyhow::{Context as _, ensure};
use wamn_oci::package_artifact::{
    PackageRegistry, Repository, registry_credentials::RegistryCredentialSource,
};

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

    /// Require the manifest of a digest-pinned workload image.
    ///
    /// # Errors
    /// Names the reference if its manifest is missing or cannot be read.
    pub async fn require_image(
        &self,
        reference: &str,
        insecure_registry: bool,
    ) -> anyhow::Result<()> {
        let (name, digest) = reference
            .rsplit_once('@')
            .with_context(|| format!("image {reference} is missing a digest"))?;
        let digest: wamn_digest::Digest = digest
            .parse()
            .with_context(|| format!("parse the digest of image {reference}"))?;
        let base = name
            .rsplit_once(':')
            .filter(|(_, tag)| !tag.contains('/'))
            .map_or(name, |(base, _)| base);
        let manifest = async {
            let mut registry = self.repository(base).await?;
            registry.insecure_registry = insecure_registry;
            Repository::open(&registry, &digest)?.manifest().await
        }
        .await
        .with_context(|| format!("read image {reference}"))?;
        ensure!(manifest.is_some(), "image {reference} is missing");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{DeployPackages, PackageRegistry};
    use wamn_digest::bytes_sha256;
    use wamn_test_infrastructure::{
        publish::{Server, fixture_registry},
        scratch::ScratchRoot,
    };

    #[tokio::test]
    async fn require_image_refuses_a_missing_manifest_and_names_the_reference() -> anyhow::Result<()>
    {
        let manifest = br#"{"schemaVersion":2}"#.to_vec();
        let digest = bytes_sha256(&manifest);
        let server = Server::start(vec![(200, manifest), (404, Vec::new())]).await;
        let scratch = ScratchRoot::create()?;
        let registry: PackageRegistry = fixture_registry(&scratch, &server);
        let reference = format!("{}@{digest}", registry.artifact_base);
        let packages = DeployPackages {
            registry_token_metadata: false,
            registry_auth_file: registry.registry_auth_file,
            insecure_registry: registry.insecure_registry,
            oci_ca_paths: Vec::new(),
        };
        let tagged = format!("{}:fixture@{digest}", registry.artifact_base);
        packages.require_image(&tagged, true).await?;

        let error = packages.require_image(&reference, true).await.unwrap_err();
        assert!(error.to_string().contains(&reference));
        Ok(())
    }
}
