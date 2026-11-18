//! Select the chart pinned by the mounted control configuration.

use super::Platform;
use crate::platform::artifact_plan;

pub(super) fn resolve_platform(platform: &mut Platform) -> anyhow::Result<()> {
    let chart = artifact_plan::select(&platform.chart)?;
    chart.directory().clone_into(&mut platform.chart);
    if chart.is_downloaded() {
        platform.chart_download = Some(std::sync::Arc::new(chart));
    }
    Ok(())
}
