//! Supplied release artifacts for the existing Receiving application cases.

use anyhow::{Context as _, ensure};
use wamn_catalog::ServingManifest;
use wamn_control::delivery::Candidate;

pub(super) use wamn_test_infrastructure::rendering::{host_values, image_reference};

/// The candidate that `qualify-release` names. A Receiving cluster case runs
/// only with one (docs/plan/platform-deploy.md §8.2).
pub(super) fn candidate() -> anyhow::Result<(Candidate, ServingManifest)> {
    let candidate = Candidate::from_env()?;
    let (manifest, _) = candidate.manifest()?;
    let result = std::env::var_os("WAMN_DELIVERY_RESULT")
        .context("WAMN_DELIVERY_RESULT is required for supplied-artifact execution")?;
    let result = std::path::Path::new(&result);
    ensure!(
        result.is_absolute() && !result.exists(),
        "the supplied-artifact result must be a new absolute path"
    );
    Ok((candidate, manifest))
}

/// The files that the cluster's lifecycle tool reads to pull the candidate's
/// images onto its nodes from a loopback registry, through the container that
/// publishes its port on the kind network. Another registry needs none.
pub(super) async fn registry_files(
    candidate: &Candidate,
    work: &std::path::Path,
) -> anyhow::Result<()> {
    let (authority, _) = candidate
        .images
        .host
        .split_once('/')
        .context("the candidate's host image has a registry authority")?;
    if let Some(endpoint) = wamn_gate_harness::case_release::registry_endpoint(authority).await? {
        wamn_gate_harness::case_release::registry_files(&endpoint, &candidate.images.host, work)?;
    }
    Ok(())
}
