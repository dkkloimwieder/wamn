//! An OCI distribution tag.

use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// An OCI distribution tag.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct OciTag(Box<str>);

/// Refusal to parse an OCI distribution tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseOciTagError;

impl fmt::Display for ParseOciTagError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("expected an OCI tag of at most 128 ASCII letters, digits, underscores, dots or hyphens")
    }
}
impl std::error::Error for ParseOciTagError {}

impl TryFrom<String> for OciTag {
    type Error = ParseOciTagError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let bytes = value.as_bytes();
        if bytes.is_empty()
            || bytes.len() > 128
            || !(bytes[0].is_ascii_alphanumeric() || bytes[0] == b'_')
            || !bytes
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
        {
            return Err(ParseOciTagError);
        }
        Ok(Self(value.into_boxed_str()))
    }
}
impl FromStr for OciTag {
    type Err = ParseOciTagError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value.to_owned())
    }
}
impl From<OciTag> for String {
    fn from(value: OciTag) -> Self {
        value.0.into()
    }
}
impl fmt::Display for OciTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl OciTag {
    /// Text for a wire or storage boundary.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(feature = "sqlx")]
impl sqlx::Type<sqlx::Postgres> for OciTag {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <String as sqlx::Type<sqlx::Postgres>>::type_info()
    }
    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <String as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}
#[cfg(feature = "sqlx")]
impl sqlx::Encode<'_, sqlx::Postgres> for OciTag {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <Box<str> as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&self.0, buf)
    }
}
#[cfg(feature = "sqlx")]
impl<'r> sqlx::Decode<'r, sqlx::Postgres> for OciTag {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        let value = <String as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Ok(Self::try_from(value)?)
    }
}

#[cfg(test)]
mod tests {
    use super::OciTag;
    #[test]
    fn parse_oci_tag() {
        let longest = "a".repeat(128);
        let too_long = "a".repeat(129);
        for (input, accepted) in [
            ("wamn_wms-2.1.0", true),
            ("_a", true),
            ("A.0-1_2", true),
            (longest.as_str(), true),
            ("", false),
            (too_long.as_str(), false),
            ("-leading", false),
            (".leading", false),
            ("has/slash", false),
            ("has space", false),
            ("é", false),
            ("a@b", false),
        ] {
            assert_eq!(input.parse::<OciTag>().is_ok(), accepted, "{input:?}");
        }
    }
}
