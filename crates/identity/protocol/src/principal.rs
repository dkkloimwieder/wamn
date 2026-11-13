//! Validated principal identities and role names.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, str::FromStr};
use uuid::Uuid;

/// A principal UUID. Parsing does not authenticate its owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PrincipalId(Uuid);

impl fmt::Display for PrincipalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for PrincipalId {
    type Err = ParseIdentityError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let id = Uuid::try_parse(value).map_err(|_| ParseIdentityError {
            field: "principal_id",
            message: "principal ID must be a hyphenated UUID",
        })?;
        if !id.hyphenated().to_string().eq_ignore_ascii_case(value) {
            return Err(ParseIdentityError {
                field: "principal_id",
                message: "principal ID must be a hyphenated UUID",
            });
        }
        Ok(Self(id))
    }
}

impl Serialize for PrincipalId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}
impl<'de> Deserialize<'de> for PrincipalId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        let id: Self = value.parse().map_err(serde::de::Error::custom)?;
        if id.to_string() != value {
            return Err(serde::de::Error::custom("principal ID must be canonical"));
        }
        Ok(id)
    }
}

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
        return Err(ParseIdentityError {
            field: "role",
            message: "role must be a lowercase slug of at most 64 bytes",
        });
    }
    Ok(RoleName(value.into()))
}

impl<'de> Deserialize<'de> for RoleName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        require_role(value).map_err(serde::de::Error::custom)
    }
}

/// An invalid identity value.
#[derive(Debug)]
pub struct ParseIdentityError {
    field: &'static str,
    message: &'static str,
}
impl ParseIdentityError {
    /// The input field that failed validation.
    pub fn field(&self) -> &'static str {
        self.field
    }
}
impl fmt::Display for ParseIdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message)
    }
}
impl std::error::Error for ParseIdentityError {}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn principal_and_role_parsers_refuse_invalid_values() {
        let id: PrincipalId = "A56F21C8-591A-4B4B-B4E9-15357C0B4A81".parse().unwrap();
        assert_eq!(id, id.to_string().parse().unwrap());
        for value in [
            "",
            "author@example.com",
            "a56f21c8591a4b4bb4e915357c0b4a81",
            "a56f21c8-591a-4b4b-b4e9-15357c0b4a8z",
        ] {
            assert!(value.parse::<PrincipalId>().is_err());
        }
        for value in ["", "two words", "_admin", "-admin"] {
            assert!(value.parse::<RoleName>().is_err());
        }
        let role: RoleName = " Project-Author ".parse().unwrap();
        assert_eq!(role, role.to_string().parse().unwrap());
    }
}
