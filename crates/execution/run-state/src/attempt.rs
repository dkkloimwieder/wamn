//! Durable effect-attempt generation facts.

use serde::{Deserialize, Serialize};

/// How exact environment generations are represented in the attempt ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GenerationFactKind {
    /// This occurrence has no portable connection requirement, so no
    /// connection or credential generation may be recorded.
    NotRequired,
    /// An environment attestation identified exact immutable generations.
    Attested,
}

impl GenerationFactKind {
    pub const fn as_sql(self) -> &'static str {
        match self {
            Self::NotRequired => "not-required",
            Self::Attested => "attested",
        }
    }

    pub fn from_sql(value: &str) -> Option<Self> {
        match value {
            "not-required" => Some(Self::NotRequired),
            "attested" => Some(Self::Attested),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::GenerationFactKind;

    #[test]
    fn generation_fact_kind_sql_round_trips() {
        for kind in [
            GenerationFactKind::NotRequired,
            GenerationFactKind::Attested,
        ] {
            assert_eq!(GenerationFactKind::from_sql(kind.as_sql()), Some(kind));
        }
    }
}
