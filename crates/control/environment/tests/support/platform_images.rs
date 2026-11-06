//! Explicit prepared chart inputs and role transfers for the kind cases.
use anyhow::{Context as _, ensure};
use std::path::{Path, PathBuf};
use wash_runtime::oci::{OciConfig, OciPullPolicy, pull_component, push_component};

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

pub async fn copy_role(from: &str, to: &str, destination: OciConfig) -> anyhow::Result<String> {
    let insecure = matches!(
        from.split([':', '/']).next(),
        Some("localhost" | "127.0.0.1")
    );
    let (bytes, _) = pull_component(
        from,
        OciConfig {
            insecure,
            ..Default::default()
        },
        OciPullPolicy::Always,
    )
    .await
    .with_context(|| format!("pull {from}"))?;
    push_component(to, &bytes, destination, None)
        .await
        .with_context(|| format!("push {to}"))
}
