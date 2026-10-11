//! Supplied release artifacts for the existing Receiving application cases.

use anyhow::{Context as _, ensure};
use wamn_catalog::ServingManifest;
use wamn_control::delivery::Candidate;

pub(super) use wamn_test_infrastructure::rendering::{host_values, image_reference};

pub(super) fn candidate() -> anyhow::Result<Option<(Candidate, ServingManifest)>> {
    let Some(candidate) = Candidate::from_env()? else {
        return Ok(None);
    };
    let (manifest, _) = candidate.manifest()?;
    // The candidate selects its journey packages (wamn-ld93.33.2): Receiving
    // alone, or Receiving and Acme. Each must be the package of this checkout.
    let packages = super::released_journey_packages()
        .map(|package| wamn_catalog::PackageCoordinate::new(package.id, package.version()))
        .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
    // The fixture takes the rest of its identity from the candidate, and it
    // publishes only the journey packages of this checkout.
    ensure!(
        manifest.release.packages == packages,
        "supplied Receiving artifacts require journey packages of this checkout only"
    );
    let result = std::env::var_os("WAMN_DELIVERY_RESULT")
        .context("WAMN_DELIVERY_RESULT is required for supplied-artifact execution")?;
    let result = std::path::Path::new(&result);
    ensure!(
        result.is_absolute() && !result.exists(),
        "the supplied-artifact result must be a new absolute path"
    );
    Ok(Some((candidate, manifest)))
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
