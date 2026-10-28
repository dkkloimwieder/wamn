//! The durability class frozen on a run at admission.
//!
//! The environment policy selects `standard`, which provides at-least-once
//! delivery through queue leases. A missing or mismatched local policy refuses
//! admission. The producer cannot select the class.

use std::fmt;

/// The effective durability class pinned on an admitted run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DurabilityClass {
    /// The default tier: plain lock-then-lease, no claim-time classification.
    #[default]
    Standard,
}

impl DurabilityClass {
    /// Parse the exact persisted durability-class literal.
    ///
    /// The literal set is frozen by the `runs_durability_class_check` CHECK in
    /// `deploy/sql/run-state.sql`; anything else is unknown data, not a class.
    pub fn from_sql(value: &str) -> Option<Self> {
        match value {
            "standard" => Some(Self::Standard),
            _ => None,
        }
    }

    /// Return the persisted durability-class literal.
    pub const fn as_sql(self) -> &'static str {
        match self {
            Self::Standard => "standard",
        }
    }
}

impl fmt::Display for DurabilityClass {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_sql())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durability_class_parsing_is_exact() {
        assert_eq!(
            DurabilityClass::from_sql("standard"),
            Some(DurabilityClass::Standard)
        );
        assert_eq!(DurabilityClass::from_sql("premium"), None);
        assert_eq!(DurabilityClass::from_sql("DURABLE"), None);
        assert_eq!(DurabilityClass::from_sql(""), None);
    }

    #[test]
    fn persisted_literals_are_frozen_and_match_the_check_constraint() {
        assert_eq!(DurabilityClass::Standard.as_sql(), "standard");
        let ddl = include_str!("../../../../deploy/sql/run-state.sql");
        assert!(
            ddl.contains("CHECK (durability_class IN ('standard'))"),
            "the schema of record no longer freezes the ruled literal set"
        );
        assert!(
            ddl.contains("durability_class text NOT NULL DEFAULT 'standard'"),
            "the schema of record no longer defaults to standard"
        );
    }
}
