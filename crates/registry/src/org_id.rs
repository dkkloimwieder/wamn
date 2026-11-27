//! Validated names of registry organizations.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

/// A lowercase organization slug of at most 40 bytes, with isolated hyphens.
/// The `wamn-` prefix is reserved for platform hosts.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct OrgId(Box<str>);

/// An organization name refused by the registry slug rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseOrgIdError;

impl fmt::Display for ParseOrgIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "expected a nonreserved lowercase organization slug of 1 to 40 bytes with isolated hyphens (prefix 'wamn-' is reserved)",
        )
    }
}
impl std::error::Error for ParseOrgIdError {}

impl TryFrom<String> for OrgId {
    type Error = ParseOrgIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if crate::identifiers::project_id_reason(&value).is_some()
            || crate::identifiers::has_reserved_wamn_prefix(&value)
            || matches!(
                value.as_str(),
                "platform" | "hosts" | "cert-manager" | "default"
            )
            || value.starts_with("kube-")
        {
            return Err(ParseOrgIdError);
        }
        Ok(Self(value.into_boxed_str()))
    }
}

impl FromStr for OrgId {
    type Err = ParseOrgIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value.to_owned())
    }
}

impl From<OrgId> for String {
    fn from(value: OrgId) -> Self {
        value.0.into()
    }
}

impl OrgId {
    /// Borrow the spelling used at external boundaries.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for OrgId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl PartialEq<OrgId> for str {
    fn eq(&self, other: &OrgId) -> bool {
        self == other.as_str()
    }
}
impl PartialEq<OrgId> for &str {
    fn eq(&self, other: &OrgId) -> bool {
        *self == other.as_str()
    }
}
impl PartialEq<String> for OrgId {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other.as_str()
    }
}
impl PartialEq<OrgId> for String {
    fn eq(&self, other: &OrgId) -> bool {
        self.as_str() == other.as_str()
    }
}
impl std::borrow::Borrow<str> for OrgId {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq<&str> for OrgId {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn parse_rule() {
        for (value, valid) in [
            ("dev".to_owned(), true),
            ("a-9".to_owned(), true),
            ("9".to_owned(), true),
            ("wamning".to_owned(), true),
            ("api".to_owned(), true),
            ("wamn".to_owned(), false),
            ("wamn-db".to_owned(), false),
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
            let parsed = value.parse::<super::OrgId>();
            assert_eq!(parsed.is_ok(), valid, "{value:?}");
        }
    }
}
