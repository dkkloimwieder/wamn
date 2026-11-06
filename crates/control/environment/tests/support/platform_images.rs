//! Explicit prepared chart input for the platform kind case.
use anyhow::{Context as _, ensure};
use std::path::{Path, PathBuf};

pub fn source_chart(_repository: &Path, _directory: &Path) -> anyhow::Result<PathBuf> {
    let chart = std::env::var_os("WAMN_RELEASE_CHART")
        .context("set WAMN_RELEASE_CHART to an explicit prepared chart")?;
    let chart = PathBuf::from(chart)
        .canonicalize()
        .context("resolve WAMN_RELEASE_CHART")?;
    ensure!(
        chart.join("Chart.yaml").is_file(),
        "WAMN_RELEASE_CHART has no Chart.yaml"
    );
    Ok(chart)
}
