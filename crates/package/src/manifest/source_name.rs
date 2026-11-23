//! The singular snake_case name in authored declarations.

use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// The singular snake_case name in authored declarations.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SourceName(Box<str>);

/// Refusal to parse the singular snake_case name in authored declarations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseSourceNameError;

impl fmt::Display for ParseSourceNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("authored name must be singular snake_case")
    }
}
impl std::error::Error for ParseSourceNameError {}

impl TryFrom<String> for SourceName {
    type Error = ParseSourceNameError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let mut bytes = value.bytes();
        if !bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
            || !bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
            || value.ends_with('_')
            || value.contains("__")
        {
            return Err(ParseSourceNameError);
        }
        Ok(Self(value.into_boxed_str()))
    }
}
impl FromStr for SourceName {
    type Err = ParseSourceNameError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value.to_owned())
    }
}
impl From<SourceName> for String {
    fn from(value: SourceName) -> Self {
        value.0.into()
    }
}
impl fmt::Display for SourceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl SourceName {
    /// Text for a wire or storage boundary.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<ParseSourceNameError> for crate::manifest::ManifestError {
    fn from(source: ParseSourceNameError) -> Self {
        Self::with_source(
            crate::manifest::ManifestErrorType::InvalidIdentity,
            "invalid authored name",
            source,
        )
    }
}
impl std::borrow::Borrow<str> for SourceName {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}
impl PartialEq<str> for SourceName {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}
impl PartialEq<&str> for SourceName {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}
impl From<SourceName> for crate::canonical_name::CanonicalName {
    fn from(value: SourceName) -> Self {
        String::from(value)
            .try_into()
            .expect("source names are canonical names")
    }
}
impl PartialEq<SourceName> for str {
    fn eq(&self, other: &SourceName) -> bool {
        self == other.as_str()
    }
}
impl PartialEq<SourceName> for &str {
    fn eq(&self, other: &SourceName) -> bool {
        *self == other.as_str()
    }
}
impl PartialEq<String> for SourceName {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other.as_str()
    }
}
impl PartialEq<SourceName> for String {
    fn eq(&self, other: &SourceName) -> bool {
        self.as_str() == other.as_str()
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn parse_rule() {
        for (input, accepted) in [
            ("a", true),
            ("record_2", true),
            ("type", true),
            ("", false),
            ("A", false),
            ("1a", false),
            ("a-b", false),
            ("a_", false),
            ("a__b", false),
            ("_a", false),
            ("é", false),
            ("a\0b", false),
        ] {
            assert_eq!(
                input.parse::<super::SourceName>().is_ok(),
                accepted,
                "{input:?}"
            );
        }
    }
}
