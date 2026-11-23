//! Validated names of registry environments.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

/// A lowercase environment slug of at most 40 bytes, with isolated hyphens.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct EnvName(String);

/// An environment name refused by the registry slug rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseEnvNameError;

impl fmt::Display for ParseEnvNameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "expected a lowercase environment slug of 1 to 40 bytes with isolated hyphens",
        )
    }
}
impl std::error::Error for ParseEnvNameError {}

impl TryFrom<String> for EnvName {
    type Error = ParseEnvNameError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if crate::identifiers::project_id_reason(&value).is_some() {
            return Err(ParseEnvNameError);
        }
        Ok(Self(value))
    }
}

impl FromStr for EnvName {
    type Err = ParseEnvNameError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value.to_owned())
    }
}

impl From<EnvName> for String {
    fn from(value: EnvName) -> Self {
        value.0
    }
}

impl EnvName {
    /// Borrow the spelling used at external boundaries.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn from_key_component(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl fmt::Display for EnvName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn parse_rule() {
        for (value, valid) in [
            ("dev".to_owned(), true),
            ("a-9".to_owned(), true),
            ("wamn".to_owned(), true),
            ("x".repeat(40), true),
            (String::new(), false),
            ("x".repeat(41), false),
            ("Dev".to_owned(), false),
            ("a_b".to_owned(), false),
            ("-a".to_owned(), false),
            ("a-".to_owned(), false),
            ("a--b".to_owned(), false),
            ("é".to_owned(), false),
        ] {
            let parsed = value.parse::<super::EnvName>();
            assert_eq!(parsed.is_ok(), valid, "{value:?}");
            if let Ok(name) = parsed {
                assert_eq!(String::from(name.clone()), value);
                assert_eq!(
                    serde_json::from_value::<super::EnvName>(serde_json::to_value(&name).unwrap())
                        .unwrap(),
                    name
                );
            }
        }
    }
}
