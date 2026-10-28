//! Evidence basis for operator resolution of intents.

use std::fmt;
use std::str::FromStr;

/// The evidence basis recorded for one operator resolution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperatorActionBasis {
    /// Evidence from the external system targeted by the effect.
    ExternalEvidence,
    /// Confirmation supplied by the effect counterparty.
    CounterpartyConfirmation,
    /// An explicit operator judgment when stronger evidence is unavailable.
    OperatorJudgment,
}

impl OperatorActionBasis {
    /// Stable persisted spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExternalEvidence => "external-evidence",
            Self::CounterpartyConfirmation => "counterparty-confirmation",
            Self::OperatorJudgment => "operator-judgment",
        }
    }
}

impl fmt::Display for OperatorActionBasis {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for OperatorActionBasis {
    type Err = InvalidOperatorActionBasis;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "external-evidence" => Ok(Self::ExternalEvidence),
            "counterparty-confirmation" => Ok(Self::CounterpartyConfirmation),
            "operator-judgment" => Ok(Self::OperatorJudgment),
            _ => Err(InvalidOperatorActionBasis {
                value: value.to_string(),
            }),
        }
    }
}

/// A value outside the frozen operator-action basis vocabulary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvalidOperatorActionBasis {
    value: String,
}

impl fmt::Display for InvalidOperatorActionBasis {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "unknown operator action basis {:?}; expected external-evidence, counterparty-confirmation, or operator-judgment",
            self.value
        )
    }
}

impl std::error::Error for InvalidOperatorActionBasis {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basis_vocabulary_is_exact() {
        for (literal, expected) in [
            ("external-evidence", OperatorActionBasis::ExternalEvidence),
            (
                "counterparty-confirmation",
                OperatorActionBasis::CounterpartyConfirmation,
            ),
            ("operator-judgment", OperatorActionBasis::OperatorJudgment),
        ] {
            assert_eq!(literal.parse(), Ok(expected));
            assert_eq!(expected.as_str(), literal);
        }
        assert!("success".parse::<OperatorActionBasis>().is_err());
        assert!("".parse::<OperatorActionBasis>().is_err());
    }
}
