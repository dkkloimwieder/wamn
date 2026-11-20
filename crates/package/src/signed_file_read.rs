//! Package-owned file reads carried by an operation's result schema.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Schema annotation populated from the verified package result declaration.
pub const SIGNED_FILE_READS: &str = "x-wamn-signed-reads";
/// Maximum signed read lifetime, in seconds.
pub const MAX_SIGNED_FILE_SECONDS: u64 = 15 * 60;

/// A package's signed read of one result field containing a file UUID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SignedFileRead {
    /// Omission selects fifteen minutes. Zero and longer lifetimes refuse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifetime_seconds: Option<u64>,
}

/// Result carrier shared by JSON codecs and typed operation exports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignedFileResultClass {
    One,
    OptionalOne,
    BoundedList,
    Page,
}

/// One verified field and its file signing request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedFileField {
    pub path: String,
    pub nullable: bool,
    pub read: SignedFileRead,
}

/// Result metadata admitted with the component and pinned in its descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedFileReads {
    pub class: SignedFileResultClass,
    pub fields: Vec<SignedFileField>,
}

/// Contextual refusal of signed result metadata.
#[derive(Debug)]
pub struct SignedFileReadError {
    detail: String,
}
impl fmt::Display for SignedFileReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.detail)
    }
}
impl std::error::Error for SignedFileReadError {}

impl SignedFileReadError {
    /// Report invalid signed result metadata.
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl SignedFileRead {
    /// Decode the effective lifetime and refuse values outside the contract.
    pub fn lifetime_seconds(&self) -> Result<u64, SignedFileReadError> {
        let seconds = self.lifetime_seconds.unwrap_or(MAX_SIGNED_FILE_SECONDS);
        if seconds == 0 || seconds > MAX_SIGNED_FILE_SECONDS {
            return Err(SignedFileReadError {
                detail: "signed file lifetime must be 1 through 900 seconds".into(),
            });
        }
        Ok(seconds)
    }
}

#[cfg(test)]
mod tests {
    use super::SignedFileRead;

    #[test]
    fn lifetime_defaults_and_bounds() {
        assert_eq!(SignedFileRead::default().lifetime_seconds().unwrap(), 900);
        assert_eq!(
            SignedFileRead {
                lifetime_seconds: Some(12)
            }
            .lifetime_seconds()
            .unwrap(),
            12
        );
        for seconds in [0, 901, u64::MAX] {
            assert!(
                SignedFileRead {
                    lifetime_seconds: Some(seconds)
                }
                .lifetime_seconds()
                .is_err()
            );
        }
    }
}
