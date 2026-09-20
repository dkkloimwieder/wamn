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
    let packages = super::JOURNEY_PACKAGES
        .iter()
        .map(|package| wamn_catalog::PackageCoordinate::new(package.id, package.version))
        .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
    ensure!(
        manifest.release.tenant_id == super::TENANT
            && manifest.release.environment == super::ENVIRONMENT
            && manifest.release.effective_release_id.get() == super::RELEASE_ID
            && manifest.release.packages == packages,
        "supplied Receiving artifacts require the receiving-route-auth/dev release 1 with the exact Receiving and Acme packages"
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

pub(super) fn registry_files(candidate: &Candidate, work: &std::path::Path) -> anyhow::Result<()> {
    let Some(endpoint) = &candidate.native_registry_endpoint else {
        return Ok(());
    };
    let (authority, _) = candidate
        .host_image
        .split_once('/')
        .context("the mapped native image has a registry authority")?;
    let scheme = if candidate.native_registry_insecure {
        "http"
    } else {
        "https"
    };
    let endpoint = serde_json::to_string(&format!("{scheme}://{endpoint}"))?;
    std::fs::write(work.join("native-registry-authority"), authority)?;
    std::fs::write(
        work.join("native-registry-hosts.toml"),
        format!(
            "server = {endpoint}\n[host.{endpoint}]\n  capabilities = [\"pull\", \"resolve\"]\n"
        ),
    )?;
    Ok(())
}
