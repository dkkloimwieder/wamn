//! PostgreSQL login identities.

use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// A PostgreSQL login name, preserved exactly without identifier truncation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct LoginName(String);

/// A name that PostgreSQL cannot store as one complete identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseLoginNameError;

impl fmt::Display for ParseLoginNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("login name must contain 1 to 63 bytes without NUL")
    }
}
impl std::error::Error for ParseLoginNameError {}

impl FromStr for LoginName {
    type Err = ParseLoginNameError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value.to_owned())
    }
}
impl TryFrom<String> for LoginName {
    type Error = ParseLoginNameError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() || value.len() > 63 || value.contains('\0') {
            return Err(ParseLoginNameError);
        }
        Ok(Self(value))
    }
}
impl From<LoginName> for String {
    fn from(value: LoginName) -> Self {
        value.0
    }
}
impl LoginName {
    /// Borrow the exact name for a PostgreSQL connection or quoted SQL identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Display for LoginName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::LoginName;

    #[test]
    fn login_parse_rule() {
        for (value, valid) in [
            (crate::PLATFORM_ROLE.to_owned(), true),
            ("a".repeat(63), true),
            ("Quoted Login".to_owned(), true),
            ("é".repeat(31), true),
            (String::new(), false),
            ("a".repeat(64), false),
            ("é".repeat(32), false),
            ("a\0b".to_owned(), false),
        ] {
            let parsed = value.parse::<LoginName>();
            assert_eq!(parsed.is_ok(), valid, "{value:?}");
            assert_eq!(LoginName::try_from(value.clone()).is_ok(), valid);
            assert_eq!(
                serde_json::from_value::<LoginName>(serde_json::json!(value)).is_ok(),
                valid
            );
            if let Ok(name) = parsed {
                assert_eq!(name.as_str(), value);
                assert_eq!(
                    serde_json::from_value::<LoginName>(serde_json::to_value(&name).unwrap())
                        .unwrap(),
                    name
                );
            }
        }
    }
}
