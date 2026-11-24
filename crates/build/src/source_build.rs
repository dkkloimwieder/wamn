//! Build the source closure selected by development.

use std::fmt;
use std::path::PathBuf;

use crate::{
    BuildMode, BuildRequest, DescribeMode, PreparedSource, build_prepared, default_output_root,
    package_output,
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

impl SourceBuilder<'_> {
    pub(crate) async fn build(
        &self,
        source: &PreparedSource,
        mode: BuildMode,
    ) -> anyhow::Result<PathBuf> {
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
