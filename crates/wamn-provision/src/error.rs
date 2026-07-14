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
    /// The tier has no dedicated cluster pair to render (wamn-q3n.6): a `trials`
    /// org lives on the shared pool, not a `<org>-prod` / `<org>-dev` pair (T3
    /// provisioning is wamn-q3n.9). Only `standard` / `dedicated` orgs get one.
    TierHasNoDedicatedPair {
        /// The tier that has no pair (`trials`).
        tier: &'static str,
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
    /// A tier move (wamn-q3n.13) targets the SAME tier the org is already on — a
    /// no-op, not a promotion.
    TierMoveNoop {
        /// The tier both sides share.
        tier: &'static str,
    },
    /// A tier move (wamn-q3n.13) would DOWNGRADE the org to a lower-isolation tier.
    /// Tier moves are one-way upgrades (T3→T2, T2→T4); data never moves down to a
    /// shared or lower-isolation tier.
    TierDowngrade {
        /// The current (higher) tier.
        from: &'static str,
        /// The requested (lower) tier.
        to: &'static str,
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
            ProvisionError::TierHasNoDedicatedPair { tier } => write!(
                f,
                "tier {tier:?} has no dedicated cluster pair: a trials org shares the pool \
                 (T3 provisioning is wamn-q3n.9)"
            ),
            ProvisionError::NameTooLong { name, max } => write!(
                f,
                "provisioned name {name:?} is {} bytes, over the {max}-byte limit: \
                 shorten the org or project id",
                name.len()
            ),
            ProvisionError::TierMoveNoop { tier } => write!(
                f,
                "tier move to {tier:?} is a no-op: the org is already on that tier"
            ),
            ProvisionError::TierDowngrade { from, to } => write!(
                f,
                "tier move {from:?} -> {to:?} is a downgrade: tier moves are one-way upgrades \
                 (T3->T2, T2->T4), never to a lower-isolation tier"
            ),
        }
    }
}

impl std::error::Error for ProvisionError {}
