//! Release qualification: the candidate the cases run, and the change checks.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use wamn_catalog::{ManifestDigest, PackageCoordinate, ServingManifest, ServingRelease};

use crate::package_artifact::PackageRegistry;
use crate::release_chart::{ImageSet, QualificationImages};

pub mod environment;
pub mod qualification;
pub mod qualification_artifact;
pub mod run_record;
pub mod sqlx;

/// The release inputs that a qualification fixture provisions and publishes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseIdentity {
    pub org: String,
    pub project: String,
    pub environment: String,
    pub tenant: String,
    pub route_host: String,
    pub packages: BTreeSet<PackageCoordinate>,
}

/// One published release and the platform it qualifies on: the manifest
/// bytes `qualify-release` pulled by the release digest, the release
/// registry that holds the release's packages and components, and the
/// release chart's images (docs/plan/platform-deploy.md §8.2).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub manifest_path: PathBuf,
    /// The release repository and its access. The packages and components
    /// are in the repositories beside it ([`sibling_bases`]).
    pub releases: PackageRegistry,
    /// The platform image set the cases deploy, from the release chart's
    /// defaults. Its digests are the key (R12).
    pub images: ImageSet,
    /// The gates and identity images the cases start and the runner, from
    /// the same defaults, outside the key (R12, R15).
    pub qualification: QualificationImages,
}

impl Candidate {
    /// Read the candidate that `qualify-release` wrote.
    pub fn read(path: &Path) -> anyhow::Result<Self> {
        let candidate: Self = serde_json::from_slice(
            &fs::read(path).with_context(|| format!("read candidate {}", path.display()))?,
        )
        .context("decode the candidate")?;
        ensure!(
            candidate.manifest_path.is_absolute(),
            "candidate manifest path must be absolute"
        );
        candidate.manifest()?;
        for image in [
            &candidate.images.host,
            &candidate.images.http,
            &candidate.images.materializer,
            &candidate.qualification.gates,
            &candidate.qualification.identity,
            &candidate.qualification.runner,
        ] {
            require_pinned_image(image)?;
        }
        Ok(candidate)
    }

    /// The candidate of a case that `qualify-release` runs.
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        std::env::var_os("WAMN_DELIVERY_CANDIDATE")
            .map(|path| Self::read(Path::new(&path)))
            .transpose()
    }

    /// Admit the canonical manifest through the existing catalog boundary.
    pub fn manifest(&self) -> anyhow::Result<(ServingManifest, ManifestDigest)> {
        let bytes = fs::read(&self.manifest_path)
            .with_context(|| format!("read manifest {}", self.manifest_path.display()))?;
        ServingManifest::from_canonical_bytes(&bytes).context("admit candidate serving manifest")
    }

    /// Require a case that publishes its own release to have published the
    /// candidate's exact bytes. Only the Receiving cases still publish; the
    /// WMS cases install the candidate and publish nothing.
    pub fn assert_manifest(&self, actual: &ServingManifest) -> anyhow::Result<()> {
        let (expected, _) = self.manifest()?;
        ensure!(
            actual.canonical_bytes() == expected.canonical_bytes(),
            "application fixture produced another release identity or manifest"
        );
        Ok(())
    }
}

/// The package and component repositories beside the release repository
/// `<registry>/<path>/releases`: `<registry>/<path>/packages` and
/// `<registry>/<path>/components`. Qualification reads the release's
/// packages and components there (docs/plan/platform-deploy.md §8.2).
///
/// # Errors
///
/// When the release repository is not `<registry>/<path>/releases`.
pub fn sibling_bases(release_artifact_base: &str) -> anyhow::Result<(String, String)> {
    let parent = release_artifact_base
        .strip_suffix("/releases")
        .filter(|parent| parent.contains('/'))
        .with_context(|| {
            format!(
                "the release repository {release_artifact_base} is not <registry>/<path>/releases"
            )
        })?;
    Ok((format!("{parent}/packages"), format!("{parent}/components")))
}

fn require_pinned_image(image: &str) -> anyhow::Result<()> {
    let (repository, digest) = image
        .rsplit_once("@sha256:")
        .context("candidate image must use an immutable repository@sha256:digest reference")?;
    ensure!(
        !repository.is_empty()
            && !repository.chars().any(char::is_whitespace)
            && digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "candidate image has an invalid immutable digest reference"
    );
    image
        .parse::<oci_client::Reference>()
        .context("parse the immutable native image reference")?;
    Ok(())
}

/// One required command and its observed outcome.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckResult {
    pub command: Vec<String>,
    pub result: String,
    pub cause: Option<String>,
}

/// The executed checks of one candidate.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Qualification {
    pub release: ServingRelease,
    pub manifest_digest: String,
    pub candidate: Candidate,
    pub checks: Vec<CheckResult>,
    pub result: String,
}

/// An executed application's observation of the exact supplied release.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationResult {
    pub candidate: Candidate,
    pub release: ServingRelease,
    pub manifest_digest: String,
    pub result: String,
}

/// Report success only after the owning application's assertions and cleanup succeed.
pub fn report_candidate_success(candidate: &Candidate) -> anyhow::Result<()> {
    let (manifest, _) = candidate.manifest()?;
    let path = std::env::var_os("WAMN_DELIVERY_RESULT")
        .context("WAMN_DELIVERY_RESULT is required for supplied-artifact execution")?;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(Path::new(&path))
        .context("create the application result without reusing an earlier result")?;
    serde_json::to_writer_pretty(
        file,
        &ApplicationResult {
            candidate: candidate.clone(),
            release: manifest.release.clone(),
            manifest_digest: manifest.digest().as_str().to_owned(),
            result: "pass".to_owned(),
        },
    )
    .context("write the executed application result")
}

#[cfg(test)]
mod tests {
    use super::{require_pinned_image, sibling_bases};

    #[test]
    fn the_packages_and_components_are_beside_the_releases() {
        assert_eq!(
            sibling_bases("registry.example:5000/wamn/releases").unwrap(),
            (
                "registry.example:5000/wamn/packages".to_owned(),
                "registry.example:5000/wamn/components".to_owned()
            )
        );
        for refused in [
            "registry.example/wamn/release",
            "registry.example/releases",
            "registry.example/wamn/releases/x",
        ] {
            assert!(sibling_bases(refused).is_err(), "accepted {refused}");
        }
    }

    #[test]
    fn deployment_images_require_immutable_named_references() {
        assert!(
            require_pinned_image(&format!(
                "registry.test/wamn-host@sha256:{}",
                "a".repeat(64)
            ))
            .is_ok()
        );
        for reference in [
            "wamn-host:dev",
            "sha256:abcd",
            "repo@sha256:abcd",
            "repo@sha256:",
            "@sha256:abcd",
        ] {
            assert!(
                require_pinned_image(reference).is_err(),
                "accepted {reference}"
            );
        }
    }
}
