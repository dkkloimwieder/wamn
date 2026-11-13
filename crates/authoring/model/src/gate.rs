//! Pure Gate judgment shared by local admission and the authenticated service.

use wamn_catalog::{
    AdmittedComponent, ComponentPackageScope, WiringDocument, validate_wiring_compatibility,
};

use crate::GateRefusal;

/// Judge existing admitted facts without executing a case or minting authority.
///
/// Callers own byte admission, authentication, and any report persistence.
pub fn judge_gate_document(
    document: &WiringDocument,
    scope: &ComponentPackageScope,
    components: &[AdmittedComponent],
) -> Result<(), GateRefusal> {
    validate_wiring_compatibility(document, scope, components).map_err(|error| {
        GateRefusal::InvalidDocument {
            detail: error.to_string(),
        }
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wamn_catalog::normalize_component_fact;

    use super::*;

    fn component() -> AdmittedComponent {
        normalize_component_fact(
            serde_json::from_value(json!({
                "scope": {"env_key": "tenant-a", "package-id": "orders", "package-version": "1.0.0"},
                "component": "transform", "interface-version": "0.1.0",
                "operations": {"run": {
                    "registered-operation": null, "committed-result-schema": null,
                    "input-ports": [{"name": "input", "schema": {}}],
                    "output-ports": [], "parameters": []
                }},
                "connections": []
            })).unwrap(),
            format!("sha256:{}", "7".repeat(64)),
            Vec::new(),
            Vec::new(),
        ).unwrap().component
    }

    fn document() -> WiringDocument {
        WiringDocument::parse(&json!({
            "format-version": "0.1", "wiring-id": "run", "version": 1,
            "entry": "node", "nodes": {"node": {
                "component": "transform", "interface-version": "0.1.0", "operation": "run"
            }}
        }))
        .unwrap()
    }

    #[test]
    fn judgment_uses_exact_facts_and_refuses_missing_operations_before_effects() {
        let component = component();
        let mut document = document();
        assert!(
            judge_gate_document(
                &document,
                &component.scope,
                std::slice::from_ref(&component)
            )
            .is_ok()
        );
        assert!(matches!(
            judge_gate_document(&document, &component.scope, &[]),
            Err(GateRefusal::InvalidDocument { .. })
        ));
        document.nodes.get_mut("node").unwrap().operation = "removed".to_owned();
        assert!(matches!(
            judge_gate_document(
                &document,
                &component.scope,
                std::slice::from_ref(&component)
            ),
            Err(GateRefusal::InvalidDocument { .. })
        ));
    }
}
