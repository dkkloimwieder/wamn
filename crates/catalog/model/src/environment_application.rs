//! The applications an environment declares
//! (docs/architecture/execution.md#workflows).
//!
//! `environment.k` names each application and the packages of the release
//! that belong to it. `wamn-ctl env apply` checks the map against the
//! release, renders it for the host beside the tenant and environment, and
//! keeps it with the Helm revision. A workflow belongs to one declared
//! application, by name.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// One declared application: the packages of the release that belong to it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationDefinition {
    pub packages: BTreeSet<String>,
    /// Package connection name to a project integration name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub connections: BTreeMap<String, String>,
}

/// Application name to its definition.
pub type EnvironmentApplications = BTreeMap<String, ApplicationDefinition>;

/// The sole application that holds `package`, or `None` for an absent or ambiguous owner.
pub fn application_for_package<'a>(
    applications: &'a EnvironmentApplications,
    package: &str,
) -> Option<&'a str> {
    let mut owners = applications
        .iter()
        .filter(|(_, definition)| definition.packages.contains(package))
        .map(|(name, _)| name.as_str());
    let application = owners.next()?;
    owners.next().is_none().then_some(application)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_package_has_one_declared_application_without_a_fallback() {
        let definition = ApplicationDefinition {
            connections: std::collections::BTreeMap::new(),
            packages: ["base".to_owned(), "overlay".to_owned()].into(),
        };
        let mut applications =
            EnvironmentApplications::from([("receiving".into(), definition.clone())]);
        for package in ["base", "overlay"] {
            assert_eq!(
                application_for_package(&applications, package),
                Some("receiving")
            );
        }
        assert_eq!(application_for_package(&applications, "missing"), None);
        applications.insert("other".into(), definition);
        assert_eq!(application_for_package(&applications, "base"), None);
    }
}
