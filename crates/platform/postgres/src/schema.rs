//! Validate PostgreSQL schema names used by platform DDL.

use std::fmt;

use postgres_protocol::escape::escape_identifier;

/// A validated schema name usable in quoted SQL and bare DDL rewrites.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BareSchemaName(String);

/// Why a value cannot be used as a bare project schema name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidBareSchemaName {
    reason: &'static str,
}

impl InvalidBareSchemaName {
    pub fn reason(&self) -> &'static str {
        self.reason
    }
}

impl fmt::Display for InvalidBareSchemaName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.reason())
    }
}

impl std::error::Error for InvalidBareSchemaName {}

impl BareSchemaName {
    /// Make sure of a schema name before it reaches generated SQL.
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidBareSchemaName> {
        let value = value.into();
        let mut bytes = value.bytes();
        let reason = if value.is_empty() {
            "identifier is empty"
        } else if value.as_bytes().contains(&0) {
            "identifier contains NUL"
        } else if value.len() > 63 {
            "identifier exceeds PostgreSQL's 63-byte limit"
        } else if !matches!(bytes.next(), Some(b'a'..=b'z' | b'_'))
            || !bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            "schema name must match the lowercase bare identifier syntax [a-z_][a-z0-9_]*"
        } else {
            return Ok(Self(value));
        };
        Err(InvalidBareSchemaName { reason })
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn quoted(&self) -> String {
        escape_identifier(&self.0)
    }
}

impl fmt::Display for BareSchemaName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::BareSchemaName;
    #[test]
    fn bare_schema_refuses_unusable_postgresql_names() {
        assert!(BareSchemaName::new(format!("s{}", "a".repeat(62))).is_ok());
        for value in [
            String::new(),
            "safe\0suffix".to_owned(),
            format!("s{}", "a".repeat(63)),
            format!("s{}", "é".repeat(32)),
            format!("s{}", "é".repeat(31)),
        ] {
            assert!(BareSchemaName::new(value).is_err());
        }
    }

    #[test]
    fn overlong_schema_inputs_cannot_alias_after_postgresql_truncation() {
        let first = format!("s{}", "a".repeat(63));
        let second = format!("s{}b", "a".repeat(62));
        assert!(BareSchemaName::new(first).is_err());
        assert!(BareSchemaName::new(second).is_err());
    }

    #[test]
    fn bare_schema_rejects_non_bare_identifiers() {
        for value in ["1bad", "Upper", "has-hyphen", "a b", "drop;schema"] {
            assert!(
                BareSchemaName::new(value).is_err(),
                "{value:?} must be rejected"
            );
        }
    }
}
