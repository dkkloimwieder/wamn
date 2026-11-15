//! Validated principal identities and role names.

use serde::{Deserialize, Deserializer, Serialize};
use std::{fmt, str::FromStr};
use uuid::Uuid;

/// A principal UUID in lowercase hyphenated wire form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(feature = "sqlx", sqlx(transparent))]
pub struct PrincipalId(Uuid);

impl fmt::Display for PrincipalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for PrincipalId {
    type Err = ParseIdentityError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || ParseIdentityError {
            field: "principal_id",
            message: "principal ID must be a lowercase hyphenated UUID",
        };
        let id = Uuid::try_parse(value).map_err(|_| invalid())?;
        let mut canonical = Uuid::encode_buffer();
        if id.hyphenated().encode_lower(&mut canonical) != value {
            return Err(invalid());
        }
        Ok(Self(id))
    }
}

impl TryFrom<String> for PrincipalId {
    type Error = ParseIdentityError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<Uuid> for PrincipalId {
    fn from(value: Uuid) -> Self {
        Self(value)
    }
}

impl From<PrincipalId> for String {
    fn from(value: PrincipalId) -> Self {
        value.to_string()
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
    fn principal_parse_rule() {
        for (value, valid) in [
            ("00000000-0000-0000-0000-000000000000", true),
            ("a56f21c8-591a-4b4b-b4e9-15357c0b4a81", true),
            ("", false),
            ("author@example.com", false),
            ("A56F21C8-591A-4B4B-B4E9-15357C0B4A81", false),
            ("a56f21c8591a4b4bb4e915357c0b4a81", false),
            ("{a56f21c8-591a-4b4b-b4e9-15357c0b4a81}", false),
            ("urn:uuid:a56f21c8-591a-4b4b-b4e9-15357c0b4a81", false),
            ("a56f21c8-591a-4b4b-b4e9-15357c0b4a8z", false),
            (" a56f21c8-591a-4b4b-b4e9-15357c0b4a81", false),
        ] {
            let parsed = value.parse::<PrincipalId>();
            assert_eq!(parsed.is_ok(), valid, "{value:?}");
            assert_eq!(PrincipalId::try_from(value.to_owned()).is_ok(), valid);
            assert_eq!(
                serde_json::from_value::<PrincipalId>(serde_json::json!(value)).is_ok(),
                valid
            );
            if let Ok(id) = parsed {
                assert_eq!(id.to_string(), value);
                assert_eq!(
                    serde_json::from_value::<PrincipalId>(serde_json::to_value(id).unwrap())
                        .unwrap(),
                    id
                );
            }
        }
    }

    #[test]
    fn role_parser_refuses_invalid_values() {
        for value in ["", "two words", "_admin", "-admin"] {
            assert!(value.parse::<RoleName>().is_err());
        }
        let role: RoleName = " Project-Author ".parse().unwrap();
        assert_eq!(role, role.to_string().parse().unwrap());
    }
}
