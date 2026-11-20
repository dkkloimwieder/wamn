//! Validated role names.

use serde::{Deserialize, Deserializer, Serialize};
use std::{fmt, str::FromStr};
use wamn_principal::ParseIdentityError;

/// A role name validated against the database's role-name rule.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct RoleName(Box<str>);
impl RoleName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl AsRef<str> for RoleName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}
impl fmt::Display for RoleName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
impl FromStr for RoleName {
    type Err = ParseIdentityError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim().to_ascii_lowercase();
        require_role(value)
    }
}

fn require_role(value: String) -> Result<RoleName, ParseIdentityError> {
    if value.is_empty()
        || value.len() > 64
        || !value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || (index > 0 && byte == b'-')
        })
    {
        return Err(ParseIdentityError::new(
            "role",
            "role must be a lowercase slug of at most 64 bytes",
        ));
    }
    Ok(RoleName(value.into()))
}

impl<'de> Deserialize<'de> for RoleName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        require_role(value).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_parser_refuses_invalid_values() {
        for value in ["", "two words", "_admin", "-admin"] {
            assert!(value.parse::<RoleName>().is_err());
        }
        let role: RoleName = " Project-Author ".parse().unwrap();
        assert_eq!(role, role.to_string().parse().unwrap());
    }
}
