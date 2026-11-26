//! Select the chart pinned by the mounted control configuration.

use super::Platform;

pub(super) fn resolve_platform(platform: &mut Platform) -> anyhow::Result<()> {
    let chart = (platform.select_chart)(&platform.chart)?;
    chart.directory().clone_into(&mut platform.chart);
    if chart.is_downloaded() {
        platform.chart_download = Some(chart);
    }
    Ok(())
}
