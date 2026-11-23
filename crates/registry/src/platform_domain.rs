//! The domain used in platform principal email addresses.

use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// The domain used in platform principal email addresses.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PlatformDomain(Box<str>);

/// Refusal to parse the domain used in platform principal email addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsePlatformDomainError;

impl fmt::Display for ParsePlatformDomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "platform domain must contain lowercase DNS labels within the DNS length limits",
        )
    }
}
impl std::error::Error for ParsePlatformDomainError {}

impl TryFrom<String> for PlatformDomain {
    type Error = ParsePlatformDomainError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() > 253
            || !value.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                    })
            })
        {
            return Err(ParsePlatformDomainError);
        }
        Ok(Self(value.into_boxed_str()))
    }
}
impl FromStr for PlatformDomain {
    type Err = ParsePlatformDomainError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value.to_owned())
    }
}
impl From<PlatformDomain> for String {
    fn from(value: PlatformDomain) -> Self {
        value.0.into()
    }
}
impl fmt::Display for PlatformDomain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl PlatformDomain {
    /// Text for a wire or storage boundary.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::PlatformDomain;
    #[test]
    fn parse_platform_domain() {
        let label = "a".repeat(64);
        let domain = ["a"; 128].join(".");
        for (input, accepted) in [
            ("example.invalid", true),
            ("localhost", true),
            ("a-1.b2.example", true),
            ("", false),
            (".", false),
            (".example", false),
            ("example.", false),
            ("exa..mple", false),
            ("-example.invalid", false),
            ("example-.invalid", false),
            ("Example.invalid", false),
            ("example.invalid/path", false),
            ("user@example.invalid", false),
            ("example invalid", false),
            (label.as_str(), false),
            (domain.as_str(), false),
        ] {
            assert_eq!(
                input.parse::<PlatformDomain>().is_ok(),
                accepted,
                "{input:?}"
            );
        }
    }
}
