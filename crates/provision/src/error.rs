//! Provisioning errors — enum variants mirroring each failure mode (SR6 house
//! rule 2: never `Error(String)`).

use std::fmt;

/// A project id could not be turned into safe database / role / Secret names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvisionError {
    /// An assembled per-project-env name exceeds the Postgres identifier /
    /// DNS-1123 label limit. Either `wamn-db-<org>--<project>--<env>--<instance>`
    /// (wamn-q3n.7, bounded by `MAX_DB_NAME_LEN`) or the namespace stem
    /// `wamn-<org>--<project>--<env>`, which is bounded by
    /// `MAX_NAMESPACE_STEM_LEN` rather than 63 because the minted instance
    /// suffix and its separator are appended after it (wamn-0h0g.13.57).
    /// `name` says which one. Shorten the org or project id.
    NameTooLong {
        /// The over-long assembled name.
        name: String,
        /// The maximum length (bytes).
        max: usize,
    },
    /// A copy where `src == dst` and no cutover was requested — a self-clone is
    /// a no-op; the same identity is only meaningfully copied as a *move* onto
    /// a different cluster (`cutover`).
    SelfCopyWithoutCutover {
        /// The triple named on both sides.
        triple: String,
    },
}

impl fmt::Display for ProvisionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProvisionError::NameTooLong { name, max } => write!(
                f,
                "provisioned name {name:?} is {} bytes, over the {max}-byte limit: \
                 shorten the org or project id",
                name.len()
            ),
            ProvisionError::SelfCopyWithoutCutover { triple } => write!(
                f,
                "src and dst are both {triple}: a self-copy is only meaningful as a move \
                 (re-run with --cutover to re-home the identity onto a different cluster)"
            ),
        }
    }
}

impl std::error::Error for ProvisionError {}
