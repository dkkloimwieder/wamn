use std::path::PathBuf;

/// A package tree and the digest of the verified package artifact it was
/// opened from. The opener holds the digest in memory and passes it here; no
/// file in the tree carries it (docs/operations/building.md#reading-a-pushed-package).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageRoot {
    pub path: PathBuf,
    pub artifact_digest: Option<wamn_digest::Digest>,
}
