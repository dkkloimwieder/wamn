//! An npm package name used by generated clients.

use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// An npm package name used by generated clients.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ClientPackageName(Box<str>);

/// Refusal to parse an npm package name used by generated clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseClientPackageNameError;

impl fmt::Display for ParseClientPackageNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("client package name must be a lowercase npm name of at most 214 bytes")
    }
}
impl std::error::Error for ParseClientPackageNameError {}

impl TryFrom<String> for ClientPackageName {
    type Error = ParseClientPackageNameError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() || value.len() > 214 {
            return Err(ParseClientPackageNameError);
        }
        let name = if let Some(scoped) = value.strip_prefix('@') {
            if scoped.matches('/').count() != 1 {
                return Err(ParseClientPackageNameError);
            }
            scoped
        } else {
            if value.contains('/') {
                return Err(ParseClientPackageNameError);
            }
            value.as_str()
        };
        for segment in name.split('/') {
            if !segment
                .bytes()
                .next()
                .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
                || !segment.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'-' | b'.' | b'_')
                })
            {
                return Err(ParseClientPackageNameError);
            }
        }
        Ok(Self(value.into_boxed_str()))
    }
}
impl FromStr for ClientPackageName {
    type Err = ParseClientPackageNameError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value.to_owned())
    }
}
impl From<ClientPackageName> for String {
    fn from(value: ClientPackageName) -> Self {
        value.0.into()
    }
}
impl fmt::Display for ClientPackageName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl ClientPackageName {
    /// Text for a wire or storage boundary.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(feature = "sqlx")]
impl sqlx::Type<sqlx::Postgres> for ClientPackageName {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <String as sqlx::Type<sqlx::Postgres>>::type_info()
    }
    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <String as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}
#[cfg(feature = "sqlx")]
impl sqlx::Encode<'_, sqlx::Postgres> for ClientPackageName {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <Box<str> as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&self.0, buf)
    }
}
#[cfg(feature = "sqlx")]
impl<'r> sqlx::Decode<'r, sqlx::Postgres> for ClientPackageName {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        let value = <String as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Ok(Self::try_from(value)?)
    }
}

#[cfg(test)]
mod tests {
    use super::ClientPackageName;
    #[test]
    fn parse_client_package_name() {
        let longest = "a".repeat(214);
        let too_long = "a".repeat(215);
        for (input, accepted) in [
            ("orders-client", true),
            ("@wamn/orders-client", true),
            ("a.b_c-1", true),
            ("0", true),
            (longest.as_str(), true),
            ("", false),
            (too_long.as_str(), false),
            ("@wamn", false),
            ("@/orders", false),
            ("@wamn/", false),
            ("Orders", false),
            ("-orders", false),
            ("a/b", false),
            ("@a/b/c", false),
            ("a b", false),
        ] {
            assert_eq!(
                input.parse::<ClientPackageName>().is_ok(),
                accepted,
                "{input:?}"
            );
        }
    }
}
