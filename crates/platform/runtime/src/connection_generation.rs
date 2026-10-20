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
            "failover-authorities": ["https://erp-backup.example/api/"],
            "tls-verification": "verify-authority",
            "tls-names": ["erp.example", "erp-backup.example"],
            "redirect-policy": "same-authority",
            "proxy-transport": null,
            "credential-set-handle": "erp-api"
        })
    }

    /// `definition_hash` is persisted in `catalog.connection_generations`, so its
    /// bytes must never move. The JSON value serializes with sorted keys. The golden
    /// value is the sha256 of the literal JSON below, computed outside this code.
    #[test]
    fn the_definition_hash_bytes_are_pinned() {
        let definition = definition();

        assert_eq!(
            serde_json::to_string(&definition).expect("JSON values always serialize"),
            concat!(
                r#"{"credential-set-handle":"erp-api","#,
                r#""failover-authorities":["https://erp-backup.example/api/"],"#,
                r#""primary-authority":"https://erp.example/api/","proxy-transport":null,"#,
                r#""redirect-policy":"same-authority","#,
                r#""tls-names":["erp.example","erp-backup.example"],"#,
                r#""tls-verification":"verify-authority"}"#,
            )
        );
        assert_eq!(
            definition_hash(&definition),
            "sha256:830ed16fe590d3f54b3e28cad656b5b39b6f7bad0ef6072005ed0e729c978237"
        );
    }
}
