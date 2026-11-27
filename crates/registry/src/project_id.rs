//! Validated names of registry projects.

use std::{fmt, str::FromStr, sync::LazyLock};

use serde::{Deserialize, Serialize};

/// The project selected when a component names none.
pub static DEFAULT_PROJECT: LazyLock<ProjectId> =
    LazyLock::new(|| "default".parse().expect("fixed default project"));

/// A lowercase project slug of at most 40 bytes, with isolated hyphens.
/// The platform reserves `wamn`, `wamn-*`, and its root path segments.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProjectId(Box<str>);

/// A project name refused by the registry slug or reservation rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseProjectIdError;

impl fmt::Display for ParseProjectIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "expected a lowercase project slug of 1 to 40 bytes with isolated hyphens and a nonreserved name",
        )
    }
}
impl std::error::Error for ParseProjectIdError {}

impl TryFrom<String> for ProjectId {
    type Error = ParseProjectIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if crate::identifiers::project_id_reason(&value).is_some()
            || crate::identifiers::project_id_is_reserved(&value)
        {
            return Err(ParseProjectIdError);
        }
        Ok(Self(value.into_boxed_str()))
    }
}

impl FromStr for ProjectId {
    type Err = ParseProjectIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value.to_owned())
    }
}

impl From<ProjectId> for String {
    fn from(value: ProjectId) -> Self {
        value.0.into()
    }
}

impl ProjectId {
    /// Borrow the spelling used at external boundaries.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProjectId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl PartialEq<ProjectId> for str {
    fn eq(&self, other: &ProjectId) -> bool {
        self == other.as_str()
    }
}
impl PartialEq<ProjectId> for &str {
    fn eq(&self, other: &ProjectId) -> bool {
        *self == other.as_str()
    }
}
impl PartialEq<String> for ProjectId {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other.as_str()
    }
}
impl PartialEq<ProjectId> for String {
    fn eq(&self, other: &ProjectId) -> bool {
        self.as_str() == other.as_str()
    }
}
impl std::borrow::Borrow<str> for ProjectId {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq<&str> for ProjectId {
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
            ("api-v2".to_owned(), true),
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
            let parsed = value.parse::<super::ProjectId>();
            assert_eq!(parsed.is_ok(), valid, "{value:?}");
        }
    }

    #[test]
    fn platform_root_segments_are_reserved() {
        for root in [
            "api",
            "password",
            "oauth",
            "assets",
            "clients",
            "platform-ui",
            "healthz",
            "invite",
            "recover",
            "reset",
        ] {
            assert!(root.parse::<super::ProjectId>().is_err(), "{root}");
            assert!(
                crate::identifiers::api_service_name_is_valid(root),
                "{root}"
            );
        }
    }
}
