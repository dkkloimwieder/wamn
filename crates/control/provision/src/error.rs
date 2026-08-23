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
    /// A non-project identity component (`org` or `env`) of a provisioned name is
    /// not a valid slug. Org and env — like the project — are separated by `--`
    /// (`project_env_database_name`) and `__` (`cdc_object_name`) into the derived
    /// database / CDC object names, so a malformed component (in particular a
    /// consecutive-hyphen run) would let two distinct triples derive one name
    /// (wamn-R27). Validated here since only `project` has its own
    /// [`ProvisionError::InvalidProjectId`].
    InvalidComponent {
        /// Which component (`"org"`, `"env"`, or `"instance"` — the minted
        /// 8-character suffix, wamn-0h0g.13.57).
        component: &'static str,
        /// The offending value.
        value: String,
        /// Why it was rejected (a stable, human-readable reason).
        reason: &'static str,
    },
    /// A **pooled** org has no dedicated clusters to render (D18): it shares the
    /// pool cluster, so `provision-org` records only its registry row and emits no
    /// `Cluster` CRs. Only a `dedicated` org owns clusters (`<org>-<owner(env)>`).
    OrgIsPooled {
        /// The shared pool the org is placed on.
        pool: String,
    },
    /// A recovery-domain owner env names no [`EnvPolicy`](wamn_control_registry::EnvPolicy)
    /// in the policy set — the cluster cannot be sized. A malformed registry
    /// (validate() flags it as `unknown-env`/`unknown-shared-with-target`).
    UnknownEnvPolicy {
        /// The owner env slug with no policy.
        name: String,
    },
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
            ProvisionError::InvalidProjectId { id, reason } => {
                write!(f, "invalid project id {id:?}: {reason}")
            }
            ProvisionError::ReservedProjectId { id } => write!(
                f,
                "reserved project id {id:?}: the `wamn` prefix is platform-reserved"
            ),
            ProvisionError::InvalidComponent {
                component,
                value,
                reason,
            } => write!(f, "invalid {component} {value:?}: {reason}"),
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
            ProvisionError::SelfCopyWithoutCutover { triple } => write!(
                f,
                "src and dst are both {triple}: a self-copy is only meaningful as a move \
                 (re-run with --cutover to re-home the identity onto a different cluster)"
            ),
        }
    }
}

impl std::error::Error for ProvisionError {}
