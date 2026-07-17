//! Provisioning errors — enum variants mirroring each failure mode (SR6 house
//! rule 2: never `Error(String)`).

use std::fmt;

/// A project id could not be turned into safe database / role / Secret names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvisionError {
    /// The project id is not a valid lowercase slug.
    InvalidProjectId {
        /// The offending id.
        id: String,
        /// Why it was rejected (a stable, human-readable reason).
        reason: &'static str,
    },
    /// The project id uses the platform-reserved `wamn` prefix (wamn-66x): the
    /// bare word `wamn` or any `wamn-…` id. The platform mints `wamn-db-…`
    /// database and Secret names, so a project id in that space would collide.
    ReservedProjectId {
        /// The offending id.
        id: String,
    },
    /// A **pooled** org has no dedicated clusters to render (D18): it shares the
    /// pool cluster, so `provision-org` records only its registry row and emits no
    /// `Cluster` CRs. Only a `dedicated` org owns clusters (`<org>-<owner(env)>`).
    OrgIsPooled {
        /// The shared pool the org is placed on.
        pool: String,
    },
    /// A recovery-domain owner env names no [`EnvPolicy`](wamn_registry::EnvPolicy)
    /// in the policy set — the cluster cannot be sized. A malformed registry
    /// (validate() flags it as `unknown-env`/`unknown-shared-with-target`).
    UnknownEnvPolicy {
        /// The owner env slug with no policy.
        name: String,
    },
    /// The assembled per-project-env name `wamn-db-<org>--<project>--<env>`
    /// (wamn-q3n.7) exceeds the Postgres identifier / DNS-1123 label limit.
    /// Shorten the org or project id.
    NameTooLong {
        /// The over-long assembled name.
        name: String,
        /// The maximum length (bytes).
        max: usize,
    },
    /// A copy request uses an axis that is first-class in the API shape but
    /// specified-not-built (`scope: subset`, `mode: live-cutover` — wamn-8df.5,
    /// docs/deployment-model.md §4).
    UnbuiltCopyAxis {
        /// Which axis (a stable label).
        axis: &'static str,
    },
    /// A copy where `src == dst` and no cutover was requested — a self-clone is
    /// a no-op; the same identity is only meaningfully copied as a *move* onto
    /// a different cluster (`cutover`).
    SelfCopyWithoutCutover {
        /// The triple named on both sides.
        triple: String,
    },
    /// A cutover copy that carries no data (`include: definition`): moving the
    /// serving identity to a dst that never received the rows abandons them.
    CutoverNeedsData,
}

impl fmt::Display for ProvisionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProvisionError::InvalidProjectId { id, reason } => {
                write!(f, "invalid project id {id:?}: {reason}")
            }
            ProvisionError::ReservedProjectId { id } => write!(
                f,
                "reserved project id {id:?}: the `wamn` prefix is platform-reserved"
            ),
            ProvisionError::OrgIsPooled { pool } => write!(
                f,
                "org is pooled on {pool:?}: it has no dedicated clusters to render \
                 (only registry placement is recorded)"
            ),
            ProvisionError::UnknownEnvPolicy { name } => write!(
                f,
                "recovery-domain owner env {name:?} names no env policy — cannot size its cluster"
            ),
            ProvisionError::NameTooLong { name, max } => write!(
                f,
                "provisioned name {name:?} is {} bytes, over the {max}-byte limit: \
                 shorten the org or project id",
                name.len()
            ),
            ProvisionError::UnbuiltCopyAxis { axis } => write!(
                f,
                "copy axis {axis} is specified but not built (docs/deployment-model.md §4)"
            ),
            ProvisionError::SelfCopyWithoutCutover { triple } => write!(
                f,
                "src and dst are both {triple}: a self-copy is only meaningful as a move \
                 (re-run with --cutover to re-home the identity onto a different cluster)"
            ),
            ProvisionError::CutoverNeedsData => write!(
                f,
                "a cutover requires the data half (include: data or both) — cutting traffic \
                 over to a dst without its rows abandons them"
            ),
        }
    }
}

impl std::error::Error for ProvisionError {}
