//! The durability class one run was admitted under (wamn-0h0g.20.1).
//!
//! The class is the runtime fact the crash floor is conditioned on. Both tiers
//! ship in one image, so a Cargo feature cannot express it: the carrier is the
//! per-run `wamn_run.runs.durability_class` column and the SOURCE is the
//! env/org policy consulted at admission, never a caller parameter — the same
//! authority rationale that keeps [`crate::CaptureMode`] off the admission
//! parameter list.
//!
//! [`DurabilityClass::Standard`] is R2's default: at-least-once redelivery with
//! plain lock-then-lease and NO claim-time effect classification.
//! [`DurabilityClass::Durable`] is the premium tier that re-enables the landed
//! crash floor at path 3's claim.
//!
//! Absent or unreadable policy resolves to `Standard`. That direction is
//! deliberate and ruled: **fail open to the cheap tier, never to durable**, so a
//! lost policy read can only under-charge, never silently enroll a run in
//! machinery its tenant did not buy.

use std::fmt;

/// The effective durability class pinned on an admitted run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DurabilityClass {
    /// The default tier: plain lock-then-lease, no claim-time classification.
    #[default]
    Standard,
    /// The premium tier: today's lock-then-classify-then-lease crash floor.
    Durable,
}

impl DurabilityClass {
    /// Parse the exact persisted durability-class literal.
    ///
    /// The literal set is frozen by the `runs_durability_class_check` CHECK in
    /// `deploy/sql/run-state.sql`; anything else is unknown data, not a class.
    pub fn from_sql(value: &str) -> Option<Self> {
        match value {
            "standard" => Some(Self::Standard),
            "durable" => Some(Self::Durable),
            _ => None,
        }
    }

    /// Parse a persisted literal, falling back to the cheap tier.
    ///
    /// This is the decode a claim path uses: an unrecognized literal must not
    /// promote a run into the premium floor, and it must not fail the claim
    /// either — the queue keeps running on the tier the platform sells by
    /// default.
    pub fn from_sql_or_default(value: &str) -> Self {
        Self::from_sql(value).unwrap_or_default()
    }

    /// Return the persisted durability-class literal.
    pub const fn as_sql(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Durable => "durable",
        }
    }

    /// Whether this class admits immutable effect evidence into the claim path.
    ///
    /// This is THE class gate. Every crash-floor arm — the eligibility
    /// predicate's `has_effect_attempt` disjunct, the claim-time classifier's
    /// `ExpiredWithAttempt` branch, and through them the `Terminalized` /
    /// `EffectAttempt` results and their drain-loop arms — is reachable only
    /// when this is true.
    pub const fn admits_effect_evidence(self) -> bool {
        matches!(self, Self::Durable)
    }
}

impl fmt::Display for DurabilityClass {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_sql())
    }
}

/// The SQL predicate every gated statement carries, verbatim.
///
/// One literal, one spelling, one place to audit. A statement that gates on
/// anything else is not gated by this decision.
pub const DURABLE_CLASS_SQL_PREDICATE: &str = "durability_class = 'durable'";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durability_class_parsing_is_exact() {
        assert_eq!(
            DurabilityClass::from_sql("standard"),
            Some(DurabilityClass::Standard)
        );
        assert_eq!(
            DurabilityClass::from_sql("durable"),
            Some(DurabilityClass::Durable)
        );
        assert_eq!(DurabilityClass::from_sql("premium"), None);
        assert_eq!(DurabilityClass::from_sql("DURABLE"), None);
        assert_eq!(DurabilityClass::from_sql(""), None);
    }

    #[test]
    fn the_absent_policy_default_is_the_cheap_tier() {
        // wamn-0h0g.20.1 froze this direction: fail open to `standard`, never to
        // `durable`. A default that flipped would enroll every run that lost its
        // policy read into machinery nobody bought.
        assert_eq!(DurabilityClass::default(), DurabilityClass::Standard);
        assert_eq!(
            DurabilityClass::from_sql_or_default("nonsense"),
            DurabilityClass::Standard
        );
        assert_eq!(
            DurabilityClass::from_sql_or_default("durable"),
            DurabilityClass::Durable
        );
        assert!(!DurabilityClass::default().admits_effect_evidence());
    }

    #[test]
    fn only_the_premium_class_admits_effect_evidence() {
        assert!(!DurabilityClass::Standard.admits_effect_evidence());
        assert!(DurabilityClass::Durable.admits_effect_evidence());
    }

    #[test]
    fn persisted_literals_are_frozen_and_match_the_check_constraint() {
        assert_eq!(DurabilityClass::Standard.as_sql(), "standard");
        assert_eq!(DurabilityClass::Durable.as_sql(), "durable");
        let ddl = include_str!("../../../../deploy/sql/run-state.sql");
        assert!(
            ddl.contains("CHECK (durability_class IN ('standard', 'durable'))"),
            "the schema of record no longer freezes the ruled literal set"
        );
        assert!(
            ddl.contains("durability_class text NOT NULL DEFAULT 'standard'"),
            "the schema of record no longer defaults to the cheap tier"
        );
    }
}
