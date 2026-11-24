//! Package artifact reads for deployment.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use wamn_component_policy::PackageManifest;
use wamn_digest::Digest;
use wamn_package::{
    PackageName, package_schema::PackageDirectory, package_version::PackageVersion,
};
use wamn_release::package_source::{OpenPackage, PackageRepository, PackageRoot, PackageSource};

use crate::package_artifact::{PackageRegistry, registry_credentials::RegistryCredentialSource};
use crate::{package_source, release_package};

/// Registry connection inputs supplied by the operation worker.
#[derive(Debug)]
pub struct DeployPackages {
    pub registry_token_metadata: bool,
    pub registry_auth_file: PathBuf,
    pub insecure_registry: bool,
    pub oci_ca_paths: Vec<PathBuf>,
}

#[async_trait::async_trait]
impl PackageSource for DeployPackages {
    async fn repository(&self, base: &str) -> anyhow::Result<Box<dyn PackageRepository>> {
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
        Ok(Box::new(PackageRegistry {
            artifact_base: base.to_owned(),
            credentials,
            registry_auth_file: self.registry_auth_file.clone(),
            insecure_registry: self.insecure_registry,
            oci_ca_paths: self.oci_ca_paths.clone(),
        }))
    }

    fn read_package_root(&self, root: &PackageRoot) -> anyhow::Result<PackageDirectory> {
        package_source::read_package_root(root)
    }

    fn migration_root(&self, path: &Path) -> PathBuf {
        package_source::migration_root(path)
    }
}

#[async_trait::async_trait]
impl PackageRepository for PackageRegistry {
    async fn open(
        &self,
        id: &PackageName,
        version: &PackageVersion,
    ) -> anyhow::Result<Box<dyn OpenPackage>> {
        Ok(Box::new(
            release_package::open_package(self, id, version).await?,
        ))
    }

    async fn open_digest(
        &self,
        digest: &Digest,
    ) -> anyhow::Result<(Box<dyn OpenPackage>, PackageManifest)> {
        let (artifact, manifest) = release_package::open_package_digest(self, digest).await?;
        Ok((Box::new(artifact), manifest))
    }
}

impl OpenPackage for release_package::OpenedPackage {
    fn read_manifest(&self) -> anyhow::Result<PackageManifest> {
        let path = wamn_codegen::package_manifest_path(self.root());
        let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        Ok(PackageManifest::from_slice(&bytes)?)
    }

    fn package_root(&self) -> PackageRoot {
        self.package_root()
    }
}
