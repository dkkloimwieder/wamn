//! Public lookup identities of personal access tokens.

use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// The eight random bytes that identify a personal access token without its secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PatPrefix([u8; 8]);

/// Refusal to parse a public token lookup identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsePatPrefixError;
impl fmt::Display for ParsePatPrefixError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("token prefix must contain 16 lowercase hexadecimal digits")
    }
}
impl std::error::Error for ParsePatPrefixError {}
impl FromStr for PatPrefix {
    type Err = ParsePatPrefixError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 16
            || !value
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(ParsePatPrefixError);
        }
        let mut bytes = [0; 8];
        hex::decode_to_slice(value, &mut bytes).map_err(|_| ParsePatPrefixError)?;
        Ok(Self(bytes))
    }
}
impl TryFrom<String> for PatPrefix {
    type Error = ParsePatPrefixError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}
impl From<PatPrefix> for String {
    fn from(value: PatPrefix) -> Self {
        value.to_string()
    }
}
impl fmt::Display for PatPrefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}
#[cfg(feature = "sqlx")]
impl sqlx::Type<sqlx::Postgres> for PatPrefix {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <String as sqlx::Type<sqlx::Postgres>>::type_info()
    }
    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <String as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}
#[cfg(feature = "sqlx")]
impl sqlx::Encode<'_, sqlx::Postgres> for PatPrefix {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <String as sqlx::Encode<sqlx::Postgres>>::encode(self.to_string(), buf)
    }
}
#[cfg(feature = "sqlx")]
impl<'r> sqlx::Decode<'r, sqlx::Postgres> for PatPrefix {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        Ok(<&str as sqlx::Decode<sqlx::Postgres>>::decode(value)?.parse()?)
    }
}
#[cfg(test)]
mod tests {
    use super::PatPrefix;
    #[test]
    fn parse_pat_prefix() {
        for (input, accepted) in [
            ("0123456789abcdef", true),
            ("0000000000000000", true),
            ("ffffffffffffffff", true),
            ("", false),
            ("0123456789abcde", false),
            ("0123456789abcdef0", false),
            ("0123456789ABCDEf", false),
            ("0123456789abcdeg", false),
            (" 123456789abcdef", false),
        ] {
            assert_eq!(input.parse::<PatPrefix>().is_ok(), accepted, "{input:?}");
        }
    }
}
