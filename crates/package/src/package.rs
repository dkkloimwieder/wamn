//! Immutable application-package coordinates and effective-release membership.

use serde::{Deserialize, Deserializer, Serialize};

use crate::PackageIdentityError;

use std::{borrow::Borrow, fmt, str::FromStr};

/// A package name in its persisted snake_case spelling.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PackageName(String);

/// A refusal to parse a package name in its persisted snake_case spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsePackageNameError;

impl fmt::Display for ParsePackageNameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("expected a snake_case package name other than wamn")
    }
}

impl std::error::Error for ParsePackageNameError {}

impl TryFrom<String> for PackageName {
    type Error = ParsePackageNameError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let mut bytes = value.bytes();
        if !bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
            || !bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
            || value.ends_with('_')
            || value.contains("__")
            || value == "wamn"
        {
            return Err(ParsePackageNameError);
        }
        Ok(Self(value))
    }
}

impl FromStr for PackageName {
    type Err = ParsePackageNameError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value.to_owned())
    }
}

impl PackageName {
    /// Borrow the wire spelling at a text boundary.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PackageName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl From<PackageName> for String {
    fn from(value: PackageName) -> Self {
        value.0
    }
}

impl AsRef<str> for PackageName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for PackageName {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

#[cfg(test)]
mod name_tests {
    use super::PackageName;

    #[test]
    fn parse_rule() {
        for (value, valid) in [
            ("orders", true),
            ("orders_2_api", true),
            ("a", true),
            ("", false),
            ("wamn", false),
            ("Orders", false),
            ("2orders", false),
            ("_orders", false),
            ("orders_", false),
            ("orders__api", false),
            ("orders-api", false),
            ("orders api", false),
            ("é", false),
            ("orders\0", false),
        ] {
            let parsed = value.parse::<PackageName>();
            assert_eq!(parsed.is_ok(), valid, "{value:?}");
            let decoded = serde_json::from_value::<PackageName>(serde_json::json!(value));
            assert_eq!(decoded.is_ok(), valid, "{value:?}");
            if let Ok(name) = parsed {
                assert_eq!(decoded.unwrap(), name);
                assert_eq!(
                    serde_json::from_value::<PackageName>(serde_json::to_value(&name).unwrap())
                        .unwrap(),
                    name
                );
            }
        }
    }
}

/// Validate one operation token against its package-owned coordinate.
pub fn validate_canonical_operation_for_package(
    value: &crate::registered_operation::RegisteredOperation,
    package_id: &crate::PackageName,
    package_version: &crate::package_version::PackageVersion,
) -> Result<(), PackageIdentityError> {
    let native_package_id = package_id.as_str().replace('_', "-");
    let expected_prefix = format!("{native_package_id}:");
    let expected_suffix = format!("@{package_version}");
    if !value.as_str().starts_with(&expected_prefix) || !value.as_str().ends_with(&expected_suffix)
    {
        return invalid(format!(
            "registered operation {value:?} does not belong to package {package_id:?} version {package_version:?}"
        ));
    }
    Ok(())
}

pub(crate) fn invalid<T>(message: impl Into<String>) -> Result<T, PackageIdentityError> {
    Err(PackageIdentityError {
        message: message.into(),
    })
}

/// One immutable package coordinate.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct PackageCoordinate {
    package_id: crate::PackageName,
    package_version: crate::package_version::PackageVersion,
}

impl<'de> Deserialize<'de> for PackageCoordinate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "kebab-case", deny_unknown_fields)]
        struct Wire {
            package_id: crate::PackageName,
            package_version: crate::package_version::PackageVersion,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.package_id, wire.package_version).map_err(serde::de::Error::custom)
    }
}

impl PackageCoordinate {
    /// Construct one exact package identity.
    pub fn new(
        package_id: crate::PackageName,
        package_version: crate::package_version::PackageVersion,
    ) -> Result<Self, PackageIdentityError> {
        Ok(Self {
            package_id,
            package_version,
        })
    }

    /// Package-local identity.
    pub fn package_id(&self) -> &crate::PackageName {
        &self.package_id
    }

    /// Exact authored package version.
    pub fn package_version(&self) -> &crate::package_version::PackageVersion {
        &self.package_version
    }
}

#[cfg(test)]
mod tests {
    use super::validate_canonical_operation_for_package;

    #[test]
    fn operation_tokens_use_the_native_component_export_grammar() {
        let token = "wamn-fixture:widget-maker/get@1.0.0".parse().unwrap();
        assert!(
            validate_canonical_operation_for_package(
                &token,
                &"wamn_fixture".parse().unwrap(),
                &"1.0.0".parse().unwrap()
            )
            .is_ok()
        );
        assert!(
            validate_canonical_operation_for_package(
                &token,
                &"other_package".parse().unwrap(),
                &"1.0.0".parse().unwrap()
            )
            .is_err()
        );
        assert!(
            validate_canonical_operation_for_package(
                &token,
                &"wamn_fixture".parse().unwrap(),
                &"2.0.0".parse().unwrap()
            )
            .is_err()
        );
    }
}
