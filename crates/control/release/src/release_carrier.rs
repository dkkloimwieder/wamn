//! The release identity one workload deployment carries.

use wamn_catalog::ManifestDigest;

/// Exact release identity carried into one workload deployment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseCarrier {
    /// Registry repository holding the immutable release artifact.
    pub artifact_base: String,
    /// The digest of the release manifest's canonical bytes.
    pub manifest_digest: ManifestDigest,
}
