//! Immutable application-package coordinates and effective-release membership.

use serde::{Deserialize, Deserializer, Serialize};

use crate::{CatalogIdentityError, validate_text};

/// Validate the repository's exact canonical application-operation identity.
pub(crate) fn validate_canonical_operation(value: &str) -> Result<(), CatalogIdentityError> {
    validate_text(value, "registered-operation")?;
    let Some((package_id, local)) = value.split_once(':') else {
        return invalid(
            "registered operation must be <package-id>:<module>/<action>@<package-version>",
        );
    };
    if local.contains(':') {
        return invalid("registered operation contains more than one package separator");
    }
    let Some((local, package_version)) = local.rsplit_once('@') else {
        return invalid("registered operation must carry an exact package version");
    };
    if local.contains('@') {
        return invalid("registered operation contains more than one version separator");
    };
    validate_kebab_identifier(package_id, "registered-operation package-id")?;
    validate_package_version(package_version)?;
    let Some((owner, action)) = local.split_once('/') else {
        return invalid("local operation must be <module>/<action>");
    };
    if action.contains('/') {
        return invalid("local operation contains more than one action separator");
    }
    validate_kebab_identifier(owner, "registered-operation owner")?;
    validate_kebab_identifier(action, "registered-operation action")
}

/// Validate one operation token against its package-owned coordinate.
pub(crate) fn validate_canonical_operation_for_package(
    value: &str,
    package_id: &str,
    package_version: &str,
) -> Result<(), CatalogIdentityError> {
    validate_canonical_operation(value)?;
    let native_package_id = package_id.replace('_', "-");
    let expected_prefix = format!("{native_package_id}:");
    let expected_suffix = format!("@{package_version}");
    if !value.starts_with(&expected_prefix) || !value.ends_with(&expected_suffix) {
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

fn validate_kebab_identifier(value: &str, field: &'static str) -> Result<(), CatalogIdentityError> {
    validate_text(value, field)?;
    let mut bytes = value.bytes();
    if !bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
        || !bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || value.ends_with('-')
        || value.contains("--")
    {
        return Err(CatalogIdentityError::NonCanonicalIdentity { field });
    }
    Ok(())
}

fn validate_package_version(value: &str) -> Result<(), CatalogIdentityError> {
    validate_text(value, "package-version")?;
    if value.contains('@') || value.contains(':') || value.contains('/') {
        return Err(CatalogIdentityError::NonCanonicalIdentity {
            field: "package-version",
        });
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
    package_version: String,
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
            package_version: String,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.package_id, wire.package_version).map_err(serde::de::Error::custom)
    }
}

impl PackageCoordinate {
    /// Construct one exact package identity.
    pub fn new(
        package_id: impl Into<String>,
        package_version: impl Into<String>,
    ) -> Result<Self, CatalogIdentityError> {
        let package_id = package_id.into();
        let package_version = package_version.into();
        validate_snake_identifier(&package_id, "package-id")?;
        validate_package_version(&package_version)?;
        Ok(Self {
            package_id,
            package_version,
        })
    }

    /// Package-local identity.
    pub fn package_id(&self) -> &str {
        &self.package_id
    }

    /// Exact authored package version.
    pub fn package_version(&self) -> &str {
        &self.package_version
    }
}

/// The environment-local integer identity of an effective release.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct EffectiveReleaseId(u32);

impl EffectiveReleaseId {
    /// Construct a non-zero effective-release identity.
    pub fn new(value: u32) -> Result<Self, CatalogIdentityError> {
        if value == 0 {
            return Err(CatalogIdentityError::ZeroVersion {
                field: "effective-release-id",
            });
        }
        Ok(Self(value))
    }

    /// Integer database carrier.
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl<'de> Deserialize<'de> for EffectiveReleaseId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u32::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        EffectiveReleaseId, PackageCoordinate, validate_canonical_operation,
        validate_canonical_operation_for_package,
    };

    #[test]
    fn persisted_coordinates_preserve_constructor_invariants() {
        assert!(
            serde_json::from_str::<PackageCoordinate>(
                r#"{"package-id":"","package-version":"1.0.0"}"#
            )
            .is_err()
        );
        for package_id in ["not-snake", "orders_", "orders__api"] {
            assert!(PackageCoordinate::new(package_id, "1.0.0").is_err());
        }
        for package_version in ["", " 1.0.0", "1.0.0 ", "1\0.0", "1@0", "1::0"] {
            assert!(PackageCoordinate::new("orders", package_version).is_err());
            let wire = serde_json::json!({
                "package-id": "orders",
                "package-version": package_version,
            });
            assert!(serde_json::from_value::<PackageCoordinate>(wire).is_err());
        }
        assert!(serde_json::from_str::<EffectiveReleaseId>("0").is_err());
    }

    #[test]
    fn operation_tokens_use_the_native_component_export_grammar() {
        let token = "wamn-receiving:purchase-order/get@1.0.0";
        assert_eq!(validate_canonical_operation(token), Ok(()));
        assert_eq!(
            validate_canonical_operation_for_package(token, "wamn_receiving", "1.0.0"),
            Ok(())
        );

        for malformed in [
            "wamn_receiving@1.0.0::purchase_order.get",
            "wamn_receiving:purchase-order/get@1.0.0",
            "wamn-receiving:purchase_order/get@1.0.0",
            "wamn-receiving:purchase-order.get@1.0.0",
            "wamn-receiving:purchase-order/get",
        ] {
            assert!(
                validate_canonical_operation(malformed).is_err(),
                "{malformed}"
            );
        }
        assert!(validate_canonical_operation_for_package(token, "other_package", "1.0.0").is_err());
        assert!(
            validate_canonical_operation_for_package(token, "wamn_receiving", "2.0.0").is_err()
        );
    }
}
