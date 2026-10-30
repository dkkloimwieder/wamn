//! Shared identities for immutable connection definitions and bindings.

use serde_json::Value;
use sha2::{Digest as _, Sha256};

/// The non-secret validation subject shared by durable and local bindings.
pub fn binding_validation_subject(
    descriptor: &wamn_catalog::ConnectionTypeDescriptor,
    requirement_hash: &str,
    definition_hash: &str,
) -> Value {
    serde_json::json!({"requirement-type": descriptor.requirement_type, "contract": descriptor.contract, "requirement-hash": requirement_hash, "definition-hash": definition_hash})
}

/// Compute the canonical `sha256:<hex>` identity of a staged JSON definition.
pub fn definition_hash(definition: &Value) -> String {
    let bytes = serde_json::to_vec(definition).expect("JSON values always serialize");
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn definition() -> Value {
        json!({
            "primary-authority": "https://erp.example/api/",
            "tls-verification": "verify-authority"
        })
    }

    /// Local admission retains `definition_hash`, so its bytes stay fixed. The JSON value serializes with sorted keys. The golden
    /// value is the sha256 of the literal JSON below, computed outside this code.
    #[test]
    fn the_definition_hash_bytes_are_pinned() {
        let definition = definition();

        assert_eq!(
            serde_json::to_string(&definition).expect("JSON values always serialize"),
            r#"{"primary-authority":"https://erp.example/api/","tls-verification":"verify-authority"}"#
        );
        assert_eq!(
            definition_hash(&definition),
            "sha256:8a2d5e57fd0a85fc2ef8136b6501b8466061654bcb541d052cc9b8f1b6bf3407"
        );
    }
}
