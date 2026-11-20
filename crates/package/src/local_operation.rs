//! A package-local module and action written in source form.

use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// A package-local module and action written in source form.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct LocalOperation(Box<str>);

/// Refusal to parse a package-local module and action written in source form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseLocalOperationError;

impl fmt::Display for ParseLocalOperationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("expected a lowercase snake-case module.action identity")
    }
}
impl std::error::Error for ParseLocalOperationError {}

impl TryFrom<String> for LocalOperation {
    type Error = ParseLocalOperationError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let (module, action) = value.split_once('.').ok_or(ParseLocalOperationError)?;
        for part in [module, action] {
            let mut bytes = part.bytes();
            if !bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
                || !bytes
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
                || part.ends_with('_')
                || part.contains("__")
            {
                return Err(ParseLocalOperationError);
            }
        }
        Ok(Self(value.into_boxed_str()))
    }
}
impl FromStr for LocalOperation {
    type Err = ParseLocalOperationError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value.to_owned())
    }
}
impl From<LocalOperation> for String {
    fn from(value: LocalOperation) -> Self {
        value.0.into()
    }
}
impl fmt::Display for LocalOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl LocalOperation {
    /// Text for a wire or storage boundary.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(feature = "sqlx")]
impl sqlx::Type<sqlx::Postgres> for LocalOperation {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <String as sqlx::Type<sqlx::Postgres>>::type_info()
    }
    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <String as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}
#[cfg(feature = "sqlx")]
impl sqlx::Encode<'_, sqlx::Postgres> for LocalOperation {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <Box<str> as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&self.0, buf)
    }
}
#[cfg(feature = "sqlx")]
impl<'r> sqlx::Decode<'r, sqlx::Postgres> for LocalOperation {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        let value = <String as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Ok(Self::try_from(value)?)
    }
}

impl std::borrow::Borrow<str> for LocalOperation {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}
#[cfg(test)]
mod tests {
    use super::LocalOperation;
    #[test]
    fn parse_local_operation() {
        for (input, accepted) in [
            ("widget.get", true),
            ("a1.x_y", true),
            ("a_b.c1", true),
            ("", false),
            ("Widget.get", false),
            ("widget", false),
            ("widget.get.more", false),
            ("widget..get", false),
            ("widget._get", false),
            ("widget.get_", false),
            ("widget.get__one", false),
            ("widget-.get", false),
        ] {
            assert_eq!(
                input.parse::<LocalOperation>().is_ok(),
                accepted,
                "{input:?}"
            );
        }
    }
}
