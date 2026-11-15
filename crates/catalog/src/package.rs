//! Immutable application-package coordinates and effective-release membership.

use serde::{Deserialize, Deserializer, Serialize};

use crate::{CatalogIdentityError, validate_text};

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

impl sqlx::Type<sqlx::Postgres> for PackageName {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <String as sqlx::Type<sqlx::Postgres>>::type_info()
    }

    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <String as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}

impl sqlx::postgres::PgHasArrayType for PackageName {
    fn array_type_info() -> sqlx::postgres::PgTypeInfo {
        <String as sqlx::postgres::PgHasArrayType>::array_type_info()
    }
}

impl<'q> sqlx::Encode<'q, sqlx::Postgres> for PackageName {
    fn encode_by_ref(
        &self,
        buffer: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <&str as sqlx::Encode<'q, sqlx::Postgres>>::encode(self.as_str(), buffer)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for PackageName {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        Ok(Self::try_from(<String as sqlx::Decode<
            'r,
            sqlx::Postgres,
        >>::decode(value)?)?)
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
    let Some((local, package_version)) = crate::split_sealed_operation(local) else {
        return invalid("registered operation must carry an exact package version");
    };
    if local.contains('@') {
        return invalid("registered operation contains more than one version separator");
    }
    crate::PackageName::try_from(package_id.replace('-', "_")).map_err(|error| {
        CatalogIdentityError::InvalidDefinition {
            message: error.to_string(),
        }
    })?;
    if package_id.contains('_') {
        return invalid("operation package name must use hyphens");
    }
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
    package_id: &crate::PackageName,
    package_version: &str,
) -> Result<(), CatalogIdentityError> {
    validate_canonical_operation(value)?;
    let native_package_id = package_id.as_str().replace('_', "-");
    let expected_prefix = format!("{native_package_id}:");
    let expected_suffix = format!("@{package_version}");
    if !value.starts_with(&expected_prefix) || !value.ends_with(&expected_suffix) {
        return invalid(format!(
            "registered operation {value:?} does not belong to package {package_id:?} version {package_version:?}"
        ));
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

pub(crate) fn validate_package_version(value: &str) -> Result<(), CatalogIdentityError> {
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
    package_id: crate::PackageName,
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
            package_id: crate::PackageName,
            package_version: String,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.package_id, wire.package_version).map_err(serde::de::Error::custom)
    }
}

impl PackageCoordinate {
    /// Construct one exact package identity.
    pub fn new(
        package_id: crate::PackageName,
        package_version: impl Into<String>,
    ) -> Result<Self, CatalogIdentityError> {
        let package_version = package_version.into();
        validate_package_version(&package_version)?;
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
    pub fn package_version(&self) -> &str {
        &self.package_version
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PackageCoordinate, validate_canonical_operation, validate_canonical_operation_for_package,
    };

    #[test]
    fn persisted_coordinates_preserve_constructor_invariants() {
        for package_version in ["", " 1.0.0", "1.0.0 ", "1\0.0", "1@0", "1::0"] {
            assert!(PackageCoordinate::new("orders".parse().unwrap(), package_version).is_err());
            let wire = serde_json::json!({
                "package-id": "orders",
                "package-version": package_version,
            });
            assert!(serde_json::from_value::<PackageCoordinate>(wire).is_err());
        }
    }

    #[test]
    fn operation_tokens_use_the_native_component_export_grammar() {
        let token = "wamn-fixture:widget-maker/get@1.0.0";
        assert_eq!(validate_canonical_operation(token), Ok(()));
        assert_eq!(
            validate_canonical_operation_for_package(
                token,
                &"wamn_fixture".parse().unwrap(),
                "1.0.0"
            ),
            Ok(())
        );

        for malformed in [
            "wamn_fixture@1.0.0::widget_maker.get",
            "wamn_fixture:widget-maker/get@1.0.0",
            "wamn-fixture:widget_maker/get@1.0.0",
            "wamn-fixture:widget-maker.get@1.0.0",
            "wamn-fixture:widget-maker/get",
        ] {
            assert!(
                validate_canonical_operation(malformed).is_err(),
                "{malformed}"
            );
        }
        assert!(
            validate_canonical_operation_for_package(
                token,
                &"other_package".parse().unwrap(),
                "1.0.0"
            )
            .is_err()
        );
        assert!(
            validate_canonical_operation_for_package(
                token,
                &"wamn_fixture".parse().unwrap(),
                "2.0.0"
            )
            .is_err()
        );
    }
}
