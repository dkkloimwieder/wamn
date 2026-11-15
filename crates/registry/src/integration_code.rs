//! The catalog code of an integration implementation.

use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// The catalog code of an integration implementation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct IntegrationCode(Box<str>);

/// Refusal to parse the catalog code of an integration implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseIntegrationCodeError;

impl fmt::Display for ParseIntegrationCodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("integration code must start with a lowercase letter and contain lowercase letters, digits, underscores or hyphens")
    }
}
impl std::error::Error for ParseIntegrationCodeError {}

impl TryFrom<String> for IntegrationCode {
    type Error = ParseIntegrationCodeError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if !value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
            || !value.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
            })
        {
            return Err(ParseIntegrationCodeError);
        }
        Ok(Self(value.into_boxed_str()))
    }
}
impl FromStr for IntegrationCode {
    type Err = ParseIntegrationCodeError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value.to_owned())
    }
}
impl From<IntegrationCode> for String {
    fn from(value: IntegrationCode) -> Self {
        value.0.into()
    }
}
impl fmt::Display for IntegrationCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl IntegrationCode {
    /// Text for a wire or storage boundary.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl sqlx::Type<sqlx::Postgres> for IntegrationCode {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <String as sqlx::Type<sqlx::Postgres>>::type_info()
    }
    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <String as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}
impl sqlx::Encode<'_, sqlx::Postgres> for IntegrationCode {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <Box<str> as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&self.0, buf)
    }
}
impl<'r> sqlx::Decode<'r, sqlx::Postgres> for IntegrationCode {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        let value = <String as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Ok(Self::try_from(value)?)
    }
}

#[cfg(test)]
mod tests {
    use super::IntegrationCode;
    #[test]
    fn parse_integration_code() {
        for (input, accepted) in [
            ("a", true),
            ("modbus", true),
            ("a_1-b", true),
            ("a-", true),
            ("a__b", true),
            ("", false),
            ("1a", false),
            ("_a", false),
            ("-a", false),
            ("Modbus", false),
            ("a/b", false),
            ("a b", false),
            ("a.b", false),
        ] {
            assert_eq!(
                input.parse::<IntegrationCode>().is_ok(),
                accepted,
                "{input:?}"
            );
        }
    }
}
