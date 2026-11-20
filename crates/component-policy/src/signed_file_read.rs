//! Admission checks for signed file results.

use crate::AdmittedComponentOperation;
use std::collections::BTreeSet;
use wamn_package::{SIGNED_FILE_READS, SignedFileReadError, SignedFileReads};

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
            return Err(SignedFileReadError::new(
                "signed file reads require one main result of a registered operation",
            ));
        }
        let reads: SignedFileReads = serde_json::from_value(value.clone()).map_err(|error| {
            SignedFileReadError::new(format!("invalid signed file result: {error}"))
        })?;
        let mut paths = BTreeSet::new();
        if reads.fields.is_empty() {
            return Err(SignedFileReadError::new(
                "signed file result contains no fields",
            ));
        }
        for field in &reads.fields {
            field.read.lifetime_seconds()?;
            if !paths.insert(&field.path) || !valid_path(&field.path) {
                return Err(SignedFileReadError::new(format!(
                    "invalid or duplicate signed file field {}",
                    field.path
                )));
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
    use super::signed_file_reads;
    use serde_json::json;

    #[test]
    fn stored_annotations_refuse_invalid_lifetimes_paths_and_palette_authority() {
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
        ] {
            let mut invalid = document.clone();
            *invalid.pointer_mut(path).unwrap() = value;
            assert!(
                signed_file_reads(&serde_json::from_value(invalid).unwrap()).is_err(),
                "{path}"
            );
        }
    }
}
