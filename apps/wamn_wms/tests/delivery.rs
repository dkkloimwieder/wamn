//! The candidate release of the WMS cluster cases.

use anyhow::{Context as _, ensure};
use wamn_control::delivery::Candidate;

pub(crate) use wamn_test_infrastructure::rendering::{host_values, image_reference};

/// The candidate that `qualify-release` names. A WMS cluster case runs only
/// with one (docs/plan/platform-deploy.md §8.2).
pub(crate) fn candidate() -> anyhow::Result<Candidate> {
    let candidate = Candidate::from_env()?;
    let (manifest, _) = candidate.manifest()?;
    ensure!(
        manifest
            .release
            .packages
            .iter()
            .map(wamn_catalog::PackageCoordinate::package_id)
            .eq(["wamn_wms"]),
        "a WMS cluster case runs a release of the WMS package alone"
    );
    let result = std::env::var_os("WAMN_DELIVERY_RESULT")
        .context("WAMN_DELIVERY_RESULT is required for supplied-artifact execution")?;
    let result = std::path::Path::new(&result);
    ensure!(
        result.is_absolute() && !result.exists(),
        "the supplied-artifact result must be a new absolute path"
    );
    Ok(candidate)
}

/// The files that the cluster's lifecycle tool reads to put `images` onto
/// its nodes: from a loopback registry, the files to pull them through the
/// container that publishes its port on the kind network; from another
/// registry, their archives, which the nodes import.
pub(crate) async fn registry_files(
    candidate: &Candidate,
    work: &std::path::Path,
    images: &[&str],
) -> anyhow::Result<()> {
    let (authority, _) = candidate
        .images
        .host
        .split_once('/')
        .context("the candidate's host image has a registry authority")?;
    if let Some(endpoint) = wamn_gate_harness::case_release::registry_endpoint(authority).await? {
        wamn_gate_harness::case_release::registry_files(&endpoint, &candidate.images.host, work)
    } else {
        wamn_gate_harness::case_release::node_image_archives(candidate, images, work).await
    }
}
