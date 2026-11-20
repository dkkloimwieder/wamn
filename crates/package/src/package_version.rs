//! The exact authored version of a package.

use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// The exact authored version of a package.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PackageVersion(Box<str>);

/// Refusal to parse the exact authored version of a package.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsePackageVersionError;

impl fmt::Display for ParsePackageVersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("package version must be canonical text without operation token separators")
    }
}
impl std::error::Error for ParsePackageVersionError {}

impl TryFrom<String> for PackageVersion {
    type Error = ParsePackageVersionError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty()
            || value.trim() != value
            || value.as_bytes().contains(&0)
            || value.contains(['@', ':', '/'])
        {
            return Err(ParsePackageVersionError);
        }
        Ok(Self(value.into_boxed_str()))
    }
}
impl FromStr for PackageVersion {
    type Err = ParsePackageVersionError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value.to_owned())
    }
}
impl From<PackageVersion> for String {
    fn from(value: PackageVersion) -> Self {
        value.0.into()
    }
}
impl fmt::Display for PackageVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl PackageVersion {
    /// Text for a wire or storage boundary.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(feature = "sqlx")]
impl sqlx::Type<sqlx::Postgres> for PackageVersion {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <String as sqlx::Type<sqlx::Postgres>>::type_info()
    }
    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <String as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}
#[cfg(feature = "sqlx")]
impl sqlx::Encode<'_, sqlx::Postgres> for PackageVersion {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <Box<str> as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&self.0, buf)
    }
}
#[cfg(feature = "sqlx")]
impl<'r> sqlx::Decode<'r, sqlx::Postgres> for PackageVersion {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        let value = <String as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Ok(Self::try_from(value)?)
    }
}

#[cfg(test)]
mod tests {
    use super::PackageVersion;
    #[test]
    fn parse_package_version() {
        for (input, accepted) in [
            ("1.0.0", true),
            ("1.0.0-dev.habcdef123456", true),
            ("build_1", true),
            ("", false),
            (" 1.0.0", false),
            ("1.0.0 ", false),
            ("1\0.0", false),
            ("1@0", false),
            ("1:0", false),
            ("1/0", false),
        ] {
            assert_eq!(
                input.parse::<PackageVersion>().is_ok(),
                accepted,
                "{input:?}"
            );
        }
    }
}
