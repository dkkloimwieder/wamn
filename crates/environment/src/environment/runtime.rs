//! Select the chart pinned by the mounted control configuration.

use super::Platform;
use wamn_kubernetes::release_chart::bundle;

pub(super) fn resolve_platform(platform: &mut Platform) -> anyhow::Result<()> {
    let chart = bundle::select(
        &platform.chart,
        &wamn_oci::platform_install::PlatformRegistry,
    )?;
    chart.directory().clone_into(&mut platform.chart);
    if chart.is_downloaded() {
        platform.chart_download = Some(std::sync::Arc::new(chart));
    }
    Ok(())
}
