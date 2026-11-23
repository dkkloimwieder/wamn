//! A versioned package-qualified operation export.

use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// A versioned package-qualified operation export.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RegisteredOperation(Box<str>);

/// Refusal to parse a versioned package-qualified operation export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseRegisteredOperationError;

impl fmt::Display for ParseRegisteredOperationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("expected <package-id>:<module>/<action>@<package-version>")
    }
}
impl std::error::Error for ParseRegisteredOperationError {}

impl TryFrom<String> for RegisteredOperation {
    type Error = ParseRegisteredOperationError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let value_ref = value.as_str();
        let Some((package_id, local)) = value_ref.split_once(':') else {
            return Err(ParseRegisteredOperationError);
        };
        let Some((local, version)) = local.split_once('@') else {
            return Err(ParseRegisteredOperationError);
        };
        let Some((owner, action)) = local.split_once('/') else {
            return Err(ParseRegisteredOperationError);
        };
        if package_id == "wamn" {
            return Err(ParseRegisteredOperationError);
        }
        version
            .parse::<crate::package_version::PackageVersion>()
            .map_err(|_| ParseRegisteredOperationError)?;
        for part in [package_id, owner, action] {
            let mut bytes = part.bytes();
            if !bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
                || !bytes
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
                || part.ends_with('-')
                || part.contains("--")
            {
                return Err(ParseRegisteredOperationError);
            }
        }
        Ok(Self(value.into_boxed_str()))
    }
}
impl FromStr for RegisteredOperation {
    type Err = ParseRegisteredOperationError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value.to_owned())
    }
}
impl From<RegisteredOperation> for String {
    fn from(value: RegisteredOperation) -> Self {
        value.0.into()
    }
}
impl fmt::Display for RegisteredOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl RegisteredOperation {
    /// Text for a wire or storage boundary.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::borrow::Borrow<str> for RegisteredOperation {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

#[cfg(test)]
mod tests {
    use super::RegisteredOperation;
    #[test]
    fn parse_registered_operation() {
        for (input, accepted) in [
            ("orders:receipt/get@1.0.0", true),
            ("wamn-control:user/list@0.9.0", true),
            ("wamn-fixture:widget-maker/get@1.0.0-dev.habc", true),
            ("", false),
            ("wamn:receipt/get@1.0.0", false),
            ("orders:receipt/get", false),
            ("orders:receipt/get@", false),
            ("orders:receipt/get@ 1.0.0", false),
            ("orders:receipt/get@1/0", false),
            ("orders:receipt/get@1:0", false),
            ("orders:receipt/get@1@0", false),
            ("orders:receipt/get@1\0.0", false),
            ("orders_api:receipt/get@1", false),
            ("orders:receipt_get/get@1", false),
            ("orders:receipt--get/get@1", false),
            ("orders:receipt/get-@1", false),
            ("orders:receipt/get/more@1", false),
        ] {
            assert_eq!(
                input.parse::<RegisteredOperation>().is_ok(),
                accepted,
                "{input:?}"
            );
        }
    }
}
