//! Shared identities for immutable connection definitions and bindings.

use serde_json::Value;

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
    wamn_digest::canonical_json_sha256(definition)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_identity_changes_with_the_validated_definition() {
        let descriptor = wamn_catalog::ConnectionTypeDescriptor::blobstore_v1();
        let original = definition_hash(&binding_validation_subject(
            &descriptor,
            "sha256:req",
            "sha256:def",
        ));
        let changed = definition_hash(&binding_validation_subject(
            &descriptor,
            "sha256:req",
            "sha256:other",
        ));
        assert_ne!(original, changed);
    }
}
