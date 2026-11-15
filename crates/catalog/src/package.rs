//! Immutable application-package coordinates and effective-release membership.

use serde::{Deserialize, Deserializer, Serialize};

use crate::{CatalogIdentityError, validate_text};

/// Validate one operation token against its package-owned coordinate.
pub(crate) fn validate_canonical_operation_for_package(
    value: &crate::registered_operation::RegisteredOperation,
    package_id: &str,
    package_version: &crate::package_version::PackageVersion,
) -> Result<(), CatalogIdentityError> {
    let native_package_id = package_id.replace('_', "-");
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

fn validate_snake_identifier(value: &str, field: &'static str) -> Result<(), CatalogIdentityError> {
    validate_text(value, field)?;
    let mut bytes = value.bytes();
    if !bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
        || !bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        || value.ends_with('_')
        || value.contains("__")
    {
        return Err(CatalogIdentityError::NonCanonicalIdentity { field });
    }
    Ok(())
}

/// The package id reserved for the platform. Its operation tokens start with
/// `wamn:`, the platform principal namespace.
const RESERVED_PACKAGE_ID: &str = "wamn";

fn refuse_reserved_package_id(value: &str) -> Result<(), CatalogIdentityError> {
    if value == RESERVED_PACKAGE_ID {
        return invalid(format!("package id {RESERVED_PACKAGE_ID:?} is reserved"));
    }
    Ok(())
}

fn invalid<T>(message: impl Into<String>) -> Result<T, CatalogIdentityError> {
    Err(CatalogIdentityError::InvalidDefinition {
        message: message.into(),
    })
}

/// One immutable package coordinate.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct PackageCoordinate {
    package_id: String,
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
            package_id: String,
            package_version: crate::package_version::PackageVersion,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.package_id, wire.package_version).map_err(serde::de::Error::custom)
    }
}

impl PackageCoordinate {
    /// Construct one exact package identity.
    pub fn new(
        package_id: impl Into<String>,
        package_version: crate::package_version::PackageVersion,
    ) -> Result<Self, CatalogIdentityError> {
        let package_id = package_id.into();
        validate_snake_identifier(&package_id, "package-id")?;
        refuse_reserved_package_id(&package_id)?;
        Ok(Self {
            package_id,
            package_version,
        })
    }

    pub(crate) fn into_parts(self) -> (String, crate::package_version::PackageVersion) {
        (self.package_id, self.package_version)
    }

    /// Package-local identity.
    pub fn package_id(&self) -> &str {
        &self.package_id
    }

    /// Exact authored package version.
    pub fn package_version(&self) -> &crate::package_version::PackageVersion {
        &self.package_version
    }
}

#[cfg(test)]
mod tests {
    use super::PackageCoordinate;

    #[test]
    fn persisted_coordinates_preserve_constructor_invariants() {
        assert!(
            serde_json::from_str::<PackageCoordinate>(
                r#"{"package-id":"","package-version":"1.0.0"}"#
            )
            .is_err()
        );
        for package_id in ["not-snake", "orders_", "orders__api"] {
            assert!(PackageCoordinate::new(package_id, "1.0.0".parse().unwrap()).is_err());
        }
    }

    #[test]
    fn package_id_wamn_is_reserved() {
        assert!(PackageCoordinate::new("wamn", "1.0.0".parse().unwrap()).is_err());
        assert!(
            serde_json::from_str::<PackageCoordinate>(
                r#"{"package-id":"wamn","package-version":"1.0.0"}"#
            )
            .is_err()
        );
        assert!(PackageCoordinate::new("wamn_fixture", "1.0.0".parse().unwrap()).is_ok());
    }
}

#[cfg(test)]
mod operation_membership_tests {
    use super::validate_canonical_operation_for_package;
    #[test]
    fn operation_tokens_use_the_native_component_export_grammar() {
        let token = "wamn-fixture:widget-maker/get@1.0.0".parse().unwrap();
        assert!(
            validate_canonical_operation_for_package(
                &token,
                "wamn_fixture",
                &"1.0.0".parse().unwrap()
            )
            .is_ok()
        );
        assert!(
            validate_canonical_operation_for_package(
                &token,
                "other_package",
                &"1.0.0".parse().unwrap()
            )
            .is_err()
        );
        assert!(
            validate_canonical_operation_for_package(
                &token,
                "wamn_fixture",
                &"2.0.0".parse().unwrap()
            )
            .is_err()
        );
    }
}
