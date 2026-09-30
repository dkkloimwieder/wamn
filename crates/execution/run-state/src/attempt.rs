//! Durable effect-attempt generation facts.

use serde::{Deserialize, Serialize};

/// How exact environment generations are represented in the attempt table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GenerationFactType {
    /// This occurrence has no portable connection requirement, so no
    /// connection or credential generation may be recorded.
    NotRequired,
    /// An environment attestation identified exact immutable generations.
    Attested,
}

impl GenerationFactType {
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
    use super::GenerationFactType;

    #[test]
    fn generation_fact_kind_sql_round_trips() {
        for kind in [
            GenerationFactType::NotRequired,
            GenerationFactType::Attested,
        ] {
            assert_eq!(GenerationFactType::from_sql(kind.as_sql()), Some(kind));
        }
    }
}
