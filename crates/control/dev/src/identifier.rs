//! Validated PostgreSQL names for local database operations.

use std::fmt;

/// A validated PostgreSQL identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Identifier(String);

/// Why an identifier cannot be used as a PostgreSQL name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidIdentifier {
    reason: &'static str,
}

impl InvalidIdentifier {
    pub fn reason(&self) -> &'static str {
        self.reason
    }
}

impl fmt::Display for InvalidIdentifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.reason)
    }
}

impl std::error::Error for InvalidIdentifier {}

impl Identifier {
    /// Validate a name before it crosses into generated SQL.
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidIdentifier> {
        let value = value.into();
        if value.is_empty() {
            return Err(InvalidIdentifier {
                reason: "identifier is empty",
            });
        }
        if value.as_bytes().contains(&0) {
            return Err(InvalidIdentifier {
                reason: "identifier contains NUL",
            });
        }
        if value.len() > 63 {
            return Err(InvalidIdentifier {
                reason: "identifier exceeds PostgreSQL's 63-byte limit",
            });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn quoted(&self) -> String {
        postgres_protocol::escape::escape_identifier(&self.0)
    }
}

/// A schema-qualified PostgreSQL name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QualifiedName {
    schema: Identifier,
    name: Identifier,
}

impl QualifiedName {
    pub fn new(schema: Identifier, name: Identifier) -> Self {
        Self { schema, name }
    }

    pub fn quoted(&self) -> String {
        format!("{}.{}", self.schema.quoted(), self.name.quoted())
    }
}

#[cfg(test)]
mod tests {
    use super::Identifier;
    #[test]
    fn refuses_unusable_database_names() {
        for value in [String::new(), "nul\0byte".to_owned(), "é".repeat(32)] {
            assert!(Identifier::new(value).is_err());
        }
        assert!(Identifier::new("é".repeat(31)).is_ok());
    }
}
