//! An eight-byte lowercase alphanumeric database instance suffix.

use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// An eight-byte lowercase alphanumeric database instance suffix.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct InstanceSuffix(Box<str>);

/// Refusal to parse an eight-byte lowercase alphanumeric database instance suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseInstanceSuffixError;

impl fmt::Display for ParseInstanceSuffixError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("instance suffix must contain exactly eight lowercase letters or digits")
    }
}
impl std::error::Error for ParseInstanceSuffixError {}

impl TryFrom<String> for InstanceSuffix {
    type Error = ParseInstanceSuffixError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() != crate::name::INSTANCE_SUFFIX_LEN
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        {
            return Err(ParseInstanceSuffixError);
        }
        Ok(Self(value.into_boxed_str()))
    }
}
impl FromStr for InstanceSuffix {
    type Err = ParseInstanceSuffixError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value.to_owned())
    }
}
impl From<InstanceSuffix> for String {
    fn from(value: InstanceSuffix) -> Self {
        value.0.into()
    }
}
impl fmt::Display for InstanceSuffix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl InstanceSuffix {
    /// Text for a wire or storage boundary.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl sqlx::Type<sqlx::Postgres> for InstanceSuffix {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <String as sqlx::Type<sqlx::Postgres>>::type_info()
    }
    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <String as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}
impl sqlx::Encode<'_, sqlx::Postgres> for InstanceSuffix {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <Box<str> as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&self.0, buf)
    }
}
impl<'r> sqlx::Decode<'r, sqlx::Postgres> for InstanceSuffix {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        let value = <String as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Ok(Self::try_from(value)?)
    }
}

#[cfg(test)]
mod tests {
    use super::InstanceSuffix;
    #[test]
    fn parse_instance_suffix() {
        for (input, accepted) in [
            ("abcdefgh", true),
            ("01234567", true),
            ("k3m9x2p7", true),
            ("", false),
            ("abcdefg", false),
            ("abcdefghi", false),
            ("Abcdefgh", false),
            ("abcdefg-", false),
            ("abc_defg", false),
            (" abcdefg", false),
            ("éabcdef", false),
        ] {
            assert_eq!(
                input.parse::<InstanceSuffix>().is_ok(),
                accepted,
                "{input:?}"
            );
        }
    }
}
