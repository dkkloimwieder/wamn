//! The applications an environment declares
//! (docs/architecture/execution.md#workflows).
//!
//! `env.yaml` names each application and the packages of the release
//! that belong to it. `wamn env apply` tests the map against the
//! release, renders it for the host beside the project and environment, and
//! keeps it with the Helm revision. A workflow belongs to one declared
//! application, by name.

use serde::{Deserialize, Serialize};

use std::{
    borrow::Borrow,
    collections::{BTreeMap, BTreeSet},
    fmt,
    str::FromStr,
};

/// A lowercase application slug of at most 40 bytes, with isolated hyphens.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AppName(String);

/// An application name refused by the registry slug rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseAppNameError;

impl fmt::Display for ParseAppNameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "expected a lowercase application slug of 1 to 40 bytes with isolated hyphens",
        )
    }
}

impl std::error::Error for ParseAppNameError {}

impl TryFrom<String> for AppName {
    type Error = ParseAppNameError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if crate::identifiers::project_id_reason(&value).is_some() {
            return Err(ParseAppNameError);
        }
        Ok(Self(value))
    }
}

impl FromStr for AppName {
    type Err = ParseAppNameError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value.to_owned())
    }
}

impl AppName {
    /// Borrow the wire spelling at a text boundary.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AppName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl From<AppName> for String {
    fn from(value: AppName) -> Self {
        value.0
    }
}

impl AsRef<str> for AppName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for AppName {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

#[cfg(test)]
mod name_tests {
    use super::AppName;

    #[test]
    fn parse_rule() {
        let max = "a".repeat(40);
        let too_long = "a".repeat(41);
        for (value, valid) in [
            ("9", true),
            ("orders-2", true),
            (max.as_str(), true),
            ("api", true),
            ("wamn-app", true),
            ("", false),
            (too_long.as_str(), false),
            ("Orders", false),
            ("sales.eu", false),
            ("orders_now", false),
            ("orders now", false),
            ("orders/now", false),
            ("orders%2fnow", false),
            ("注文", false),
            ("orders\0", false),
            ("-orders", false),
            ("orders-", false),
            ("orders--now", false),
        ] {
            let parsed = value.parse::<AppName>();
            assert_eq!(parsed.is_ok(), valid, "{value:?}");
            let decoded = serde_json::from_value::<AppName>(serde_json::json!(value));
            assert_eq!(decoded.is_ok(), valid, "{value:?}");
            if let Ok(name) = parsed {
                assert_eq!(decoded.unwrap(), name);
                assert_eq!(
                    serde_json::from_value::<AppName>(serde_json::to_value(&name).unwrap())
                        .unwrap(),
                    name
                );
            }
        }
    }
}

/// One declared application: the packages of the release that belong to it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppDefinition {
    pub packages: BTreeSet<wamn_package::PackageName>,
    /// Package connection name to a project integration name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub connections: BTreeMap<String, String>,
}

/// Application name to its definition.
pub type EnvApps = BTreeMap<crate::AppName, AppDefinition>;

/// The sole application that holds `package`, or `None` for an absent or ambiguous owner.
pub fn app_for_package<'a>(
    apps: &'a EnvApps,
    package: &wamn_package::PackageName,
) -> Option<&'a crate::AppName> {
    let mut owners = apps
        .iter()
        .filter(|(_, definition)| definition.packages.contains(package))
        .map(|(name, _)| name);
    let app = owners.next()?;
    owners.next().is_none().then_some(app)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_package_has_one_declared_app_without_a_fallback() {
        let definition = AppDefinition {
            connections: std::collections::BTreeMap::new(),
            packages: ["base".parse().unwrap(), "overlay".parse().unwrap()].into(),
        };
        let mut apps = EnvApps::from([("receiving".parse().unwrap(), definition.clone())]);
        let owner = "receiving".parse().unwrap();
        for package in ["base", "overlay"] {
            let package = package.parse().unwrap();
            assert_eq!(app_for_package(&apps, &package), Some(&owner));
        }
        assert_eq!(app_for_package(&apps, &"missing".parse().unwrap()), None);
        apps.insert("other".parse().unwrap(), definition);
        assert_eq!(app_for_package(&apps, &"base".parse().unwrap()), None);
    }
}
