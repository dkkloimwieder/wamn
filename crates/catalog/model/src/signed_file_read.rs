//! Package-owned file reads carried by an operation's result schema.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::AdmittedComponentOperation;

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

/// Read and validate the operation's admitted result annotation.
pub fn signed_file_reads(
    operation: &AdmittedComponentOperation,
) -> Result<Option<SignedFileReads>, SignedFileReadError> {
    let mut selected = None;
    for port in &operation.output_ports {
        let Some(value) = port.schema.schema.get(SIGNED_FILE_READS) else {
            continue;
        };
        if port.name != "main" || selected.is_some() || operation.registered_operation.is_none() {
            return Err(SignedFileReadError {
                detail: "signed file reads require one main result of a registered operation"
                    .into(),
            });
        }
        let reads: SignedFileReads =
            serde_json::from_value(value.clone()).map_err(|error| SignedFileReadError {
                detail: format!("invalid signed file result: {error}"),
            })?;
        let mut paths = BTreeSet::new();
        if reads.fields.is_empty() {
            return Err(SignedFileReadError {
                detail: "signed file result contains no fields".into(),
            });
        }
        for field in &reads.fields {
            field.read.lifetime_seconds()?;
            if !paths.insert(&field.path) || !valid_path(&field.path) {
                return Err(SignedFileReadError {
                    detail: format!("invalid or duplicate signed file field {}", field.path),
                });
            }
        }
        selected = Some(reads);
    }
    Ok(selected)
}

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.split('.').all(|segment| {
            let name = segment.strip_suffix("[]").unwrap_or(segment);
            !name.is_empty()
                && name.bytes().enumerate().all(|(i, byte)| {
                    byte == b'_' || byte.is_ascii_lowercase() || (i > 0 && byte.is_ascii_digit())
                })
        })
}

#[cfg(test)]
mod tests {
    use super::{SignedFileRead, signed_file_reads};
    use serde_json::json;

    #[test]
    fn stored_annotations_refuse_unknown_fields_lifetimes_and_palette_authority() {
        let document = json!({"registered-operation": "app:file/read@1.0.0", "input-ports": [], "parameters": [],
        "output-ports": [{"name": "main", "schema": {"schema": {"x-wamn-signed-reads": {
            "class": "one", "fields": [{"path": "file_id", "nullable": false, "read": {}}]
        }}}}]});
        let operation = serde_json::from_value(document.clone()).unwrap();
        assert!(signed_file_reads(&operation).unwrap().is_some());
        for (path, value) in [
            ("/registered-operation", serde_json::Value::Null),
            ("/output-ports/0/name", json!("error")),
            (
                "/output-ports/0/schema/schema/x-wamn-signed-reads/fields/0/read",
                json!({"lifetime_seconds": 901}),
            ),
            (
                "/output-ports/0/schema/schema/x-wamn-signed-reads/fields/0/path",
                json!(""),
            ),
            (
                "/output-ports/0/schema/schema/x-wamn-signed-reads",
                json!({"unknown": []}),
            ),
        ] {
            let mut invalid = document.clone();
            *invalid.pointer_mut(path).unwrap() = value;
            assert!(
                signed_file_reads(&serde_json::from_value(invalid).unwrap()).is_err(),
                "{path}"
            );
        }
    }

    #[test]
    fn lifetime_and_decoding_fail_closed() {
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
        for value in [
            json!({"lifetime": 1}),
            json!({"lifetime_seconds": -1}),
            json!({"lifetime_seconds": 1.5}),
        ] {
            assert!(serde_json::from_value::<SignedFileRead>(value).is_err());
        }
    }
}
