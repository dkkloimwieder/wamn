//! The applications an environment declares
//! (docs/architecture/execution.md#workflows).
//!
//! `environment.k` names each application and the packages of the release
//! that belong to it. `wamn-ctl env apply` tests the map against the
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

/// A nonempty application name stored as SQL text.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ApplicationName(String);

/// A refusal to parse a nonempty application name stored as SQL text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseApplicationNameError;

impl fmt::Display for ParseApplicationNameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("expected a nonempty application name without a NUL byte")
    }
}

impl std::error::Error for ParseApplicationNameError {}

impl TryFrom<String> for ApplicationName {
    type Error = ParseApplicationNameError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() || value.as_bytes().contains(&0) {
            return Err(ParseApplicationNameError);
        }
        Ok(Self(value))
    }
}

impl FromStr for ApplicationName {
    type Err = ParseApplicationNameError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value.to_owned())
    }
}

impl ApplicationName {
    /// Borrow the wire spelling at a text boundary.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ApplicationName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl From<ApplicationName> for String {
    fn from(value: ApplicationName) -> Self {
        value.0
    }
}

impl AsRef<str> for ApplicationName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for ApplicationName {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl sqlx::Type<sqlx::Postgres> for ApplicationName {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <String as sqlx::Type<sqlx::Postgres>>::type_info()
    }

    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <String as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}

impl sqlx::postgres::PgHasArrayType for ApplicationName {
    fn array_type_info() -> sqlx::postgres::PgTypeInfo {
        <String as sqlx::postgres::PgHasArrayType>::array_type_info()
    }
}

impl<'q> sqlx::Encode<'q, sqlx::Postgres> for ApplicationName {
    fn encode_by_ref(
        &self,
        buffer: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <&str as sqlx::Encode<'q, sqlx::Postgres>>::encode(self.as_str(), buffer)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for ApplicationName {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        Ok(Self::try_from(<String as sqlx::Decode<
            'r,
            sqlx::Postgres,
        >>::decode(value)?)?)
    }
}

#[cfg(test)]
mod name_tests {
    use super::ApplicationName;

    #[test]
    fn parse_rule() {
        for (value, valid) in [
            ("orders", true),
            ("sales.eu", true),
            ("Order processing", true),
            ("注文", true),
            (" ", true),
            ("", false),
            ("orders\0", false),
        ] {
            let parsed = value.parse::<ApplicationName>();
            assert_eq!(parsed.is_ok(), valid, "{value:?}");
            let decoded = serde_json::from_value::<ApplicationName>(serde_json::json!(value));
            assert_eq!(decoded.is_ok(), valid, "{value:?}");
            if let Ok(name) = parsed {
                assert_eq!(decoded.unwrap(), name);
                assert_eq!(
                    serde_json::from_value::<ApplicationName>(serde_json::to_value(&name).unwrap())
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
pub struct ApplicationDefinition {
    pub packages: BTreeSet<crate::PackageName>,
    /// Package connection name to a project integration name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub connections: BTreeMap<String, String>,
}

/// Application name to its definition.
pub type EnvironmentApplications = BTreeMap<crate::ApplicationName, ApplicationDefinition>;

/// The sole application that holds `package`, or `None` for an absent or ambiguous owner.
pub fn application_for_package<'a>(
    applications: &'a EnvironmentApplications,
    package: &crate::PackageName,
) -> Option<&'a crate::ApplicationName> {
    let mut owners = applications
        .iter()
        .filter(|(_, definition)| definition.packages.contains(package))
        .map(|(name, _)| name);
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
            packages: ["base".parse().unwrap(), "overlay".parse().unwrap()].into(),
        };
        let mut applications =
            EnvironmentApplications::from([("receiving".parse().unwrap(), definition.clone())]);
        let owner = "receiving".parse().unwrap();
        for package in ["base", "overlay"] {
            let package = package.parse().unwrap();
            assert_eq!(
                application_for_package(&applications, &package),
                Some(&owner)
            );
        }
        assert_eq!(
            application_for_package(&applications, &"missing".parse().unwrap()),
            None
        );
        applications.insert("other".parse().unwrap(), definition);
        assert_eq!(
            application_for_package(&applications, &"base".parse().unwrap()),
            None
        );
    }
}
