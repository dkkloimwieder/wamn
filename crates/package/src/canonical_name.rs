//! The canonical name in component and graph facts.

use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// The canonical name in component and graph facts.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CanonicalName(Box<str>);

/// Refusal to parse the canonical name in component and graph facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseCanonicalNameError;

impl fmt::Display for ParseCanonicalNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("catalog name must be nonempty text without surrounding whitespace or NUL")
    }
}
impl std::error::Error for ParseCanonicalNameError {}

impl TryFrom<String> for CanonicalName {
    type Error = ParseCanonicalNameError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() || value.trim() != value || value.as_bytes().contains(&0) {
            return Err(ParseCanonicalNameError);
        }
        Ok(Self(value.into_boxed_str()))
    }
}
impl FromStr for CanonicalName {
    type Err = ParseCanonicalNameError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value.to_owned())
    }
}
impl From<CanonicalName> for String {
    fn from(value: CanonicalName) -> Self {
        value.0.into()
    }
}
impl fmt::Display for CanonicalName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl CanonicalName {
    /// Text for a wire or storage boundary.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::borrow::Borrow<str> for CanonicalName {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}
impl PartialEq<str> for CanonicalName {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}
impl PartialEq<&str> for CanonicalName {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}
impl PartialEq<CanonicalName> for str {
    fn eq(&self, other: &CanonicalName) -> bool {
        self == other.as_str()
    }
}
impl PartialEq<CanonicalName> for &str {
    fn eq(&self, other: &CanonicalName) -> bool {
        *self == other.as_str()
    }
}
impl PartialEq<String> for CanonicalName {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other.as_str()
    }
}
impl PartialEq<CanonicalName> for String {
    fn eq(&self, other: &CanonicalName) -> bool {
        self.as_str() == other.as_str()
    }
}
impl From<crate::registered_operation::RegisteredOperation> for CanonicalName {
    fn from(value: crate::registered_operation::RegisteredOperation) -> Self {
        String::from(value)
            .try_into()
            .expect("registered operations are canonical names")
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn parse_rule() {
        for (input, accepted) in [
            ("a", true),
            ("port-name", true),
            ("wamn:node/handler@0.2.0", true),
            ("a b", true),
            ("a\nb", true),
            ("é", true),
            ("", false),
            (" a", false),
            ("a ", false),
            ("a\0b", false),
        ] {
            assert_eq!(
                input.parse::<super::CanonicalName>().is_ok(),
                accepted,
                "{input:?}"
            );
        }
    }
}
