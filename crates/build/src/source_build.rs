//! Prepare and build the source closure selected by development.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use wamn_codegen::fingerprint;
use wamn_component_policy::PackageManifest;
use wamn_dev::source_deploy::{PackageSource, SourceBuild};
use wamn_package::{
    PackageName,
    manifest::{BaseDependencyRequirement, PackageIdentity, source_name::SourceName},
};

use crate::{
    BuildDatabase, BuildMode, BuildRequest, DescribeMode, PreparedSource, build_prepared,
    default_output_root, package_output,
};

/// Build source with the caller's database adapter and describe settings.
pub struct SourceBuilder<'a> {
    pub database: &'a dyn BuildDatabase,
    pub describe: anyhow::Result<DescribeMode<'a>>,
}

impl fmt::Debug for SourceBuilder<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceBuilder")
            .finish_non_exhaustive()
    }
}

/// A prepared source retained for the whole development pass.
#[derive(Debug)]
pub struct PreparedPackage(PreparedSource);

impl SourceBuild for SourceBuilder<'_> {
    type Prepared = PreparedPackage;

    async fn prepare(&self, root: &Path) -> anyhow::Result<PreparedPackage> {
        Ok(PreparedPackage(crate::prepare_source(root).await?))
    }

    async fn build(&self, source: &PreparedPackage, mode: BuildMode) -> anyhow::Result<PathBuf> {
        let source = &source.0;
        let output = default_output_root(source.root());
        let describe = *self
            .describe
            .as_ref()
            .map_err(|error| anyhow::anyhow!("{error:#}"))?;
        let request = BuildRequest::new(source.root(), &output, describe);
        let built = build_prepared(self.database, &request, source, mode).await?;
        Ok(package_output(&output, &built.package.id))
    }

    fn inputs(&self, source: &PreparedPackage) -> anyhow::Result<BTreeSet<PathBuf>> {
        crate::build_inputs(&source.0)
    }

    fn source_dependency(&self, root: &Path, id: &PackageName) -> anyhow::Result<PathBuf> {
        wamn_codegen::source_root(&package_output(&default_output_root(root), id))
    }

    fn built_root(&self, root: &Path) -> PathBuf {
        wamn_codegen::output_root(root)
    }

    fn built_dependency(&self, parent: &Path, id: &PackageName) -> PathBuf {
        package_output(parent, id)
    }

    fn read_built(&self, root: &Path) -> anyhow::Result<PackageManifest> {
        let marker = fingerprint::read(root)?;
        let (bytes, _) = fingerprint::read_artifact(root, &marker, Path::new("wamn.json"))?;
        Ok(PackageManifest::from_slice(&bytes)?)
    }
}

impl PackageSource for PreparedPackage {
    fn root(&self) -> &Path {
        self.0.root()
    }

    fn package(&self) -> &PackageIdentity {
        self.0.package()
    }

    fn bases(&self) -> &BTreeMap<SourceName, BaseDependencyRequirement> {
        self.0.bases()
    }
}
