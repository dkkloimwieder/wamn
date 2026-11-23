//! The catalog code of an integration implementation.

use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// The catalog code of an integration implementation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct IntegrationCode(Box<str>);

/// Refusal to parse the catalog code of an integration implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseIntegrationCodeError;

impl fmt::Display for ParseIntegrationCodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("integration code must start with a lowercase letter and contain lowercase letters, digits, underscores or hyphens")
    }
}
impl std::error::Error for ParseIntegrationCodeError {}

impl TryFrom<String> for IntegrationCode {
    type Error = ParseIntegrationCodeError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if !value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
            || !value.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
            })
        {
            return Err(ParseIntegrationCodeError);
        }
        Ok(Self(value.into_boxed_str()))
    }
}
impl FromStr for IntegrationCode {
    type Err = ParseIntegrationCodeError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value.to_owned())
    }
}
impl From<IntegrationCode> for String {
    fn from(value: IntegrationCode) -> Self {
        value.0.into()
    }
}
impl fmt::Display for IntegrationCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl IntegrationCode {
    /// Text for a wire or storage boundary.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::IntegrationCode;
    #[test]
    fn parse_integration_code() {
        for (input, accepted) in [
            ("a", true),
            ("modbus", true),
            ("a_1-b", true),
            ("a-", true),
            ("a__b", true),
            ("", false),
            ("1a", false),
            ("_a", false),
            ("-a", false),
            ("Modbus", false),
            ("a/b", false),
            ("a b", false),
            ("a.b", false),
        ] {
            assert_eq!(
                input.parse::<IntegrationCode>().is_ok(),
                accepted,
                "{input:?}"
            );
        }
    }
}
