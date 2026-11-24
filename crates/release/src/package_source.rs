use std::{
    fmt::Debug,
    path::{Path, PathBuf},
};

use wamn_component_policy::PackageManifest;
use wamn_digest::Digest;
use wamn_package::{
    PackageName, package_schema::PackageDirectory, package_version::PackageVersion,
};

/// A package tree and the digest of the verified package artifact it was
/// opened from. The opener holds the digest in memory and passes it here; no
/// file in the tree carries it (docs/operations/building.md#reading-a-pushed-package).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageRoot {
    pub path: PathBuf,
    pub artifact_digest: Option<wamn_digest::Digest>,
}

/// A verified package tree that remains available for the lifetime of the reader.
pub trait OpenPackage: Debug + Send + Sync {
    fn read_manifest(&self) -> anyhow::Result<PackageManifest>;
    fn package_root(&self) -> PackageRoot;
}

/// Read packages from one resolved registry repository.
#[async_trait::async_trait]
pub trait PackageRepository: Send + Sync {
    async fn open(
        &self,
        id: &PackageName,
        version: &PackageVersion,
    ) -> anyhow::Result<Box<dyn OpenPackage>>;
    async fn open_digest(
        &self,
        digest: &Digest,
    ) -> anyhow::Result<(Box<dyn OpenPackage>, PackageManifest)>;
}

/// Open a repository and read a verified or local package tree.
#[async_trait::async_trait]
pub trait PackageSource: Debug + Send + Sync {
    async fn repository(&self, base: &str) -> anyhow::Result<Box<dyn PackageRepository>>;
    fn read_package_root(&self, root: &PackageRoot) -> anyhow::Result<PackageDirectory>;
    fn migration_root(&self, path: &Path) -> PathBuf;
}
