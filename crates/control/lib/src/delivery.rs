//! Repository commands and exact artifact inputs for release qualification.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use wamn_catalog::{ManifestDigest, ServingManifest, ServingRelease};

pub mod deployment;
pub mod publication;
pub mod qualification;
pub mod sqlx;

/// Exact minted release and explicit artifact locations captured for qualification.
#[derive(Clone, Debug)]
pub struct PrepareReleaseRequest {
    /// URL to the database holding the minted release snapshot.
    pub database_url: String,
    /// Tenant claim carried by the minted release snapshot.
    pub tenant: String,
    /// Integer identity of the minted effective release snapshot.
    pub effective_release_id: u32,
    /// The `<registry>/<repository>` the release manifest was pushed to.
    pub artifact_base: String,
    pub target_directory: PathBuf,
    pub manifest_output: PathBuf,
    pub candidate_output: PathBuf,
    pub host_image: String,
    pub gates_image: Option<String>,
    pub identity_image: Option<String>,
    pub native_registry_endpoint: Option<String>,
    pub native_registry_insecure: bool,
    pub deployment_files: Vec<PathBuf>,
}

/// Write machine inputs from an immutable snapshot without qualifying or publishing it.
pub async fn prepare(request: PrepareReleaseRequest) -> anyhow::Result<()> {
    ensure!(
        request.manifest_output.is_absolute() && request.candidate_output.is_absolute(),
        "candidate output paths must be absolute"
    );
    ensure!(
        !request.manifest_output.exists() && !request.candidate_output.exists(),
        "candidate output paths must be unused"
    );
    let snapshot = crate::print_release_env::lookup_release_snapshot(
        &request.database_url,
        &request.tenant,
        request.effective_release_id,
        &request.artifact_base,
    )
    .await?;
    let candidate = Candidate {
        manifest_path: request.manifest_output,
        target_directory: fs::canonicalize(request.target_directory)?,
        host_image: request.host_image,
        gates_image: request.gates_image,
        identity_image: request.identity_image,
        deployment_files: request.deployment_files,
        native_registry_endpoint: request.native_registry_endpoint,
        native_registry_insecure: request.native_registry_insecure,
    };
    let mut manifest = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&candidate.manifest_path)?;
    std::io::Write::write_all(&mut manifest, &snapshot.manifest.canonical_bytes())?;
    candidate.artifact_hashes()?;
    let output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(request.candidate_output)?;
    serde_json::to_writer_pretty(output, &candidate).context("write the candidate artifact inputs")
}

/// Locations of already built artifacts, with the existing application manifest.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub manifest_path: PathBuf,
    pub target_directory: PathBuf,
    pub host_image: String,
    pub gates_image: Option<String>,
    pub identity_image: Option<String>,
    #[serde(default)]
    pub deployment_files: Vec<PathBuf>,
    #[serde(default)]
    pub native_registry_endpoint: Option<String>,
    #[serde(default)]
    pub native_registry_insecure: bool,
}

impl Candidate {
    /// Read explicit artifact inputs without building or publishing anything.
    pub fn read(path: &Path) -> anyhow::Result<Self> {
        let candidate: Self = serde_json::from_slice(
            &fs::read(path).with_context(|| format!("read candidate {}", path.display()))?,
        )
        .context("decode candidate artifact inputs")?;
        ensure!(
            candidate.manifest_path.is_absolute(),
            "candidate manifest path must be absolute"
        );
        ensure!(
            candidate.target_directory.is_absolute() && candidate.target_directory.is_dir(),
            "candidate target directory must name an existing absolute directory"
        );
        candidate.validate_registry()?;
        candidate.manifest()?;
        for image in std::iter::once(&candidate.host_image)
            .chain(candidate.gates_image.iter())
            .chain(candidate.identity_image.iter())
        {
            require_pinned_image(image)?;
        }
        Ok(candidate)
    }

    fn validate_registry(&self) -> anyhow::Result<()> {
        ensure!(
            !self.native_registry_insecure || self.native_registry_endpoint.is_some(),
            "an insecure native registry requires an explicit endpoint"
        );
        if let Some(endpoint) = &self.native_registry_endpoint {
            let parsed = url::Url::parse(&format!("http://{endpoint}"))?;
            ensure!(
                parsed.host_str().is_some()
                    && parsed.port().is_some()
                    && parsed.username().is_empty()
                    && parsed.password().is_none()
                    && parsed.path() == "/"
                    && parsed.query().is_none()
                    && parsed.fragment().is_none(),
                "the native registry endpoint must be an explicit host and port"
            );
            let host = self.host_image.parse::<oci_client::Reference>()?;
            ensure!(
                self.host_image.split('/').next() == Some(host.registry()),
                "an explicit native registry endpoint requires an explicit image registry authority"
            );
            for image in self.gates_image.iter().chain(self.identity_image.iter()) {
                ensure!(
                    image.parse::<oci_client::Reference>()?.registry() == host.registry()
                        && image.split('/').next() == Some(host.registry()),
                    "an explicit native registry endpoint requires one image registry authority"
                );
            }
        }
        Ok(())
    }

    /// Read the explicit supplied-artifact mode for an existing application case.
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

    /// Require the fresh fixture to reproduce the candidate's exact release bytes.
    pub fn assert_manifest(&self, actual: &ServingManifest) -> anyhow::Result<()> {
        let (expected, _) = self.manifest()?;
        ensure!(
            actual.canonical_bytes() == expected.canonical_bytes(),
            "application fixture produced another release identity or manifest"
        );
        Ok(())
    }

    /// Record the guest and native files that the existing application cases consume.
    pub fn artifact_hashes(&self) -> anyhow::Result<BTreeMap<PathBuf, String>> {
        self.validate_registry()?;
        for image in std::iter::once(&self.host_image)
            .chain(self.gates_image.iter())
            .chain(self.identity_image.iter())
        {
            require_pinned_image(image)?;
        }
        let mut files = BTreeMap::new();
        for path in &self.deployment_files {
            ensure!(
                path.is_absolute(),
                "deployment input paths must be absolute"
            );
            files.insert(path.clone(), file_digest(path)?);
        }
        files.insert(
            self.manifest_path.clone(),
            file_digest(&self.manifest_path)?,
        );
        for relative in ["virtualized/std-empty-environment", "wasm32-wasip2/release"] {
            let directory = self.target_directory.join(relative);
            for entry in fs::read_dir(&directory)
                .with_context(|| format!("read candidate guests {}", directory.display()))?
            {
                let path = entry?.path();
                if path
                    .extension()
                    .is_some_and(|extension| extension == "wasm")
                    && path.is_file()
                {
                    files.insert(path.clone(), file_digest(&path)?);
                }
            }
        }
        for name in [
            "wamn",
            "wamn-ctl",
            "wamn-host",
            "wamn-identity",
            "wamn-cdc-reader",
            "wamn-scenario-worker",
        ] {
            for profile in ["debug", "release"] {
                let path = self.target_directory.join(profile).join(name);
                if path.is_file() {
                    files.insert(path.clone(), file_digest(&path)?);
                }
            }
        }
        let (manifest, _) = self.manifest()?;
        for component in &manifest.components {
            ensure!(
                files
                    .values()
                    .any(|digest| digest == component.digest.as_str()),
                "candidate lacks component {} with digest {}",
                component.component,
                component.digest
            );
        }
        Ok(files)
    }
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

fn file_digest(path: &Path) -> anyhow::Result<String> {
    let bytes =
        fs::read(path).with_context(|| format!("read candidate artifact {}", path.display()))?;
    ensure!(
        !bytes.is_empty(),
        "candidate artifact is empty: {}",
        path.display()
    );
    Ok(format!(
        "sha256:{}",
        hex::encode(ring::digest::digest(&ring::digest::SHA256, &bytes))
    ))
}

/// One required command and its observed outcome.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckResult {
    pub command: Vec<String>,
    pub result: String,
    pub cause: Option<String>,
}

/// Temporary results bind executed checks to one source and exact artifact set.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Qualification {
    pub source_commit: String,
    pub release: ServingRelease,
    pub manifest_digest: String,
    pub candidate: Candidate,
    pub artifact_hashes: BTreeMap<PathBuf, String>,
    pub checks: Vec<CheckResult>,
    pub result: String,
}

impl Qualification {
    /// Read a result without granting it publication or deployment authority.
    pub fn read(path: &Path) -> anyhow::Result<Self> {
        serde_json::from_slice(&fs::read(path).context("read the qualification result")?)
            .context("decode the qualification result")
    }

    /// Refuse failures, missing required commands, and incomplete results.
    pub fn require_pass(&self) -> anyhow::Result<()> {
        ensure!(
            self.result == "pass",
            "the release did not pass qualification"
        );
        ensure!(
            self.source_commit.len() == 40
                && self
                    .source_commit
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit()),
            "qualification requires the exact source commit"
        );
        ensure!(
            !self.checks.is_empty()
                && self
                    .checks
                    .iter()
                    .all(|check| check.result == "pass" && check.cause.is_none()),
            "every required qualification command must execute and pass"
        );
        let (manifest, digest) = self.candidate.manifest()?;
        ensure!(
            self.release == manifest.release && self.manifest_digest == digest.as_str(),
            "qualification names another release or manifest digest"
        );
        qualification::require_complete_checks(self)?;
        self.assert_artifacts()
    }

    /// Refuse artifact changes after qualification.
    pub fn assert_artifacts(&self) -> anyhow::Result<()> {
        ensure!(
            self.candidate.artifact_hashes()? == self.artifact_hashes,
            "qualified artifacts changed or disappeared"
        );
        Ok(())
    }
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
pub fn report_candidate_success(
    candidate: &Candidate,
    manifest: &ServingManifest,
) -> anyhow::Result<()> {
    candidate.assert_manifest(manifest)?;
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
    use super::require_pinned_image;

    #[test]
    fn registry_mapping_refuses_implicit_insecure_and_credential_endpoints() {
        let mut candidate = super::Candidate {
            manifest_path: "unused".into(),
            target_directory: "unused".into(),
            host_image: format!("localhost:5000/host@sha256:{}", "a".repeat(64)),
            gates_image: None,
            identity_image: None,
            deployment_files: Vec::new(),
            native_registry_endpoint: None,
            native_registry_insecure: true,
        };
        assert!(candidate.validate_registry().is_err());
        for endpoint in [
            "user:password@owned-registry:5000",
            "owned-registry:5000/path",
            "owned-registry:5000?query=1",
        ] {
            candidate.native_registry_endpoint = Some(endpoint.to_owned());
            assert!(candidate.validate_registry().is_err());
        }
        candidate.native_registry_endpoint = Some("owned-registry:5000".to_owned());
        assert!(candidate.validate_registry().is_ok());
        candidate.identity_image = Some(format!("another.test/identity@sha256:{}", "a".repeat(64)));
        assert!(candidate.validate_registry().is_err());
        candidate.identity_image = None;
        candidate.host_image = format!("acme/host@sha256:{}", "a".repeat(64));
        assert!(candidate.validate_registry().is_err());
        candidate.host_image = format!("docker.io/acme/host@sha256:{}", "a".repeat(64));
        assert!(candidate.validate_registry().is_ok());
        candidate.identity_image = Some(format!("acme/identity@sha256:{}", "a".repeat(64)));
        assert!(candidate.validate_registry().is_err());
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
