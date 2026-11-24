//! Prepare and read the platform Helm release chart.

pub mod bundle;
#[cfg(test)]
mod test_util;

use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use serde::Deserialize;
use serde_yaml::Value;
use wamn_platform_install::release_chart::{ChartImageValidation, ImageSet};

use crate::helm::run_text;

/// The platform version: `version` in the `Chart.yaml` of the release chart
/// directory `chart`. The chart is its one owner, so a binary does not change
/// with the version (wamn-6qo7v).
pub fn chart_version(chart: &Path) -> anyhow::Result<String> {
    read_platform_version(&chart.join("Chart.yaml"))
}

/// Read the platform version from the release chart's top-level version.
///
/// # Errors
///
/// Returns an error naming the path if the chart cannot be read or decoded.
pub fn read_platform_version(path: &Path) -> anyhow::Result<String> {
    #[derive(Deserialize)]
    struct Chart {
        version: String,
    }

    let source = std::fs::read_to_string(path)
        .with_context(|| format!("read platform chart {}", path.display()))?;
    let chart: Chart = serde_yaml::from_str(&source).with_context(|| {
        format!(
            "read top-level version from platform chart {}",
            path.display()
        )
    })?;
    anyhow::ensure!(
        !chart.version.trim().is_empty(),
        "platform chart {} has an empty version",
        path.display()
    );
    Ok(chart.version)
}

/// Copy `chart` to `out` with the image set written into its defaults.
/// Host runtime defaults belong to the environment chart.
pub fn stamp(
    chart: &Path,
    set: &ImageSet,
    out: &Path,
    images: &impl ChartImageValidation,
) -> anyhow::Result<PathBuf> {
    images.require_host_tag(&set.host)?;
    let pinned = [("ingress", &set.ingress), ("workflow", &set.workflow)];
    for (name, image) in pinned {
        ensure!(
            image.contains("@sha256:"),
            "the {name} image {image} is not pinned by digest"
        );
    }
    copy_dir(chart, out)?;
    let path = out.join("values.yaml");
    let mut values: Value = serde_yaml::from_str(
        &std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?,
    )
    .context("parse the chart's default values")?;
    let platform = &mut values["platform"];
    platform["host"]["image"] = set.host.clone().into();
    platform["roles"]["ingress"]["image"] = set.ingress.clone().into();
    platform["workflow"]["image"] = set.workflow.clone().into();
    std::fs::write(&path, serde_yaml::to_string(&values)?)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(out.to_owned())
}

/// The default values of `chart`, with `helm show values`.
async fn default_values(chart: &Path) -> anyhow::Result<Value> {
    let shown = run_text(&["show".as_ref(), "values".as_ref(), chart.as_os_str()]).await?;
    serde_yaml::from_str(&shown).context("parse the chart's values")
}

/// A stamped default of the chart, which must not be empty.
fn stamped(value: &Value, name: &str) -> anyhow::Result<String> {
    let text = value.as_str().unwrap_or_default();
    ensure!(
        !text.is_empty(),
        "the chart's default {name} is not stamped"
    );
    Ok(text.to_owned())
}

/// Read the image set from the default values of `chart`.
pub async fn image_set(chart: &Path) -> anyhow::Result<ImageSet> {
    let values = default_values(chart).await?;
    let host = stamped(&values["platform"]["host"]["image"], "host image")?;
    let roles = &values["platform"]["roles"];
    Ok(ImageSet {
        host,
        ingress: stamped(&roles["ingress"]["image"], "ingress image")?,
        workflow: stamped(&values["platform"]["workflow"]["image"], "workflow image")?,
    })
}

fn copy_dir(from: &Path, to: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(to).with_context(|| format!("create {}", to.display()))?;
    for entry in std::fs::read_dir(from).with_context(|| format!("read {}", from.display()))? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)
                .with_context(|| format!("copy {}", entry.path().display()))?;
        }
    }
    Ok(())
}
