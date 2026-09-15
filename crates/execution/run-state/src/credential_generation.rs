//! The two reusable credential-generation slots every workload family rotates through.

use serde::{Deserialize, Serialize};

/// One of the two reusable credential-generation slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CredentialGeneration {
    A,
    B,
}

impl CredentialGeneration {
    pub const fn other(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::A => "a",
            Self::B => "b",
        }
    }
}

impl std::str::FromStr for CredentialGeneration {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "a" => Ok(Self::A),
            "b" => Ok(Self::B),
            _ => Err("credential generation must be a or b"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CredentialGeneration;

    #[test]
    fn each_generation_names_the_other() {
        assert_eq!(CredentialGeneration::A.other(), CredentialGeneration::B);
        assert_eq!(CredentialGeneration::B.other(), CredentialGeneration::A);
    }
}
