//! Upload the local platform UI from its selected build artifact.

use anyhow::{Context as _, ensure};
use std::path::{Path, PathBuf};
use wamn_package_control::web_artifact::{Sink, write_platform_ui};

/// Default destination for standalone platform UI uploads.
pub const PLATFORM_UI_BUCKET: &str = "gs://wamn-dev-web";

/// Upload the local platform UI only when its digest matches the stamped chart.
/// The current directory is the repository whose platform UI was built.
pub async fn upload_platform_ui(
    repository: &Path,
    chart: &Path,
    org: &str,
    bucket: &str,
) -> anyhow::Result<String> {
    let (dist, root) = platform_ui_source(repository, chart)?;
    let sink = Sink::parse(bucket)?;
    let root = [sink.prefix.trim_matches('/'), root.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    let store = sink.store()?;
    write_platform_ui(store.as_ref(), &root, &dist, org).await?;
    Ok(format!("{}{}/{root}/", sink.scheme.prefix(), sink.bucket))
}

pub(crate) fn platform_ui_source(
    repository: &Path,
    chart: &Path,
) -> anyhow::Result<(PathBuf, String)> {
    use crate::platform_build::{
        BUILD_RECORD_FILE, PlatformBuildRecord, platform_ui_digest, platform_ui_dist,
    };
    let build_record_path = chart.join(BUILD_RECORD_FILE);
    let build_record: PlatformBuildRecord =
        serde_json::from_slice(&std::fs::read(&build_record_path).with_context(|| {
            format!(
                "read {}; run `wamn-ctl build-platform`",
                build_record_path.display()
            )
        })?)?;
    let expected = build_record
        .platform_ui
        .context("the platform build record does not select the platform UI")?;
    let version = wamn_release_control::release_chart::chart_version(chart)?;
    let platform = wamn_release_control::release_chart::chart_version(
        &repository.join("deploy/platform/release"),
    )?;
    ensure!(
        version == platform,
        "the stamped chart version {version} differs from platform {platform}"
    );
    let dist = platform_ui_dist(repository);
    ensure!(
        platform_ui_digest(&dist)? == expected,
        "the platform UI build {} differs from the platform build record {}",
        dist.display(),
        build_record_path.display()
    );
    Ok((dist, format!("platform-ui/{version}")))
}
