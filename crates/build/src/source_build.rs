//! Prepare and build the source closure selected by development.

use std::collections::BTreeMap;
#[cfg(target_os = "linux")]
use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use wamn_package::manifest::{BaseDependencyRequirement, PackageIdentity, source_name::SourceName};

use crate::{
    BuildMode, BuildRequest, DescribeMode, PreparedSource, build_prepared, default_output_root,
    package_output, source_deploy::PackageSource,
};

/// Build source with the caller's describe settings.
pub struct SourceBuilder<'a> {
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

impl SourceBuilder<'_> {
    pub(crate) async fn build(
        &self,
        source: &PreparedPackage,
        mode: BuildMode,
    ) -> anyhow::Result<PathBuf> {
        let source = &source.0;
        let output = default_output_root(source.root());
        let describe = *self
            .describe
            .as_ref()
            .map_err(|error| anyhow::anyhow!("{error:#}"))?;
        let request = BuildRequest::new(source.root(), &output, describe);
        let built = build_prepared(&request, source, mode).await?;
        Ok(package_output(&output, &built.package.id))
    }
}

pub(crate) async fn prepare(root: &Path) -> anyhow::Result<PreparedPackage> {
    Ok(PreparedPackage(crate::prepare_source(root).await?))
}

#[cfg(target_os = "linux")]
impl PreparedPackage {
    pub(crate) fn inputs(&self) -> anyhow::Result<BTreeSet<PathBuf>> {
        crate::build_inputs(&self.0)
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
