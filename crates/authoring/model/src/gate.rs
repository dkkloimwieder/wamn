//! Pure Gate judgment shared by local admission and the authenticated service.

use std::collections::BTreeSet;

use wamn_catalog::{
    AdmittedComponent, ComponentPackageScope, WiringDocument, validate_wiring_compatibility,
};

use crate::GateRefusal;

/// Refuse malformed cases before reading any component posture.
pub fn validate_gate_cases(document: &WiringDocument) -> Result<(), GateRefusal> {
    if !document.cases.is_empty() {
        wamn_execution_contract::validate_cases(&document.cases).map_err(|error| {
            GateRefusal::InvalidTestSet {
                detail: error.to_string(),
            }
        })?;
    }
    Ok(())
}

/// Judge existing admitted facts without executing a case or minting authority.
///
/// Callers own byte admission, authentication, and any report persistence.
pub fn judge_gate_document(
    document: &WiringDocument,
    scope: &ComponentPackageScope,
    components: &[AdmittedComponent],
) -> Result<(), GateRefusal> {
    validate_gate_cases(document)?;
    validate_wiring_compatibility(document, scope, components).map_err(|error| {
        GateRefusal::InvalidDocument {
            detail: error.to_string(),
        }
    })?;
    if document.cases.is_empty() {
        return Ok(());
    }
    let effectful = document
        .nodes
        .values()
        .flat_map(|node| {
            components.iter().filter(move |component| {
                &component.scope == scope
                    && component.component == node.component
                    && component.interface_version == node.interface_version
                    && component.operations.contains_key(&node.operation)
                    && !component.effects.is_empty()
            })
        })
        .map(|component| component.component.clone())
        .collect::<BTreeSet<_>>();
    if !effectful.is_empty() {
        return Err(GateRefusal::EffectfulComponentReached {
            components: effectful.into_iter().collect(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wamn_catalog::{
        AdmittedComponentEffect, ComponentEffectProvenance, normalize_component_fact,
    };

    use super::*;

    fn component() -> AdmittedComponent {
        normalize_component_fact(
            serde_json::from_value(json!({
                "scope": {"tenant-id": "tenant-a", "package-id": "orders", "package-version": "1.0.0"},
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
            }},
            "cases": [{"case-id": "one", "input": {}, "expect": {"outcome": "responded", "status": 200}}]
        })).unwrap()
    }

    #[test]
    fn judgment_uses_exact_facts_and_refuses_missing_operations_before_effects() {
        let component = component();
        let mut document = document();
        assert!(
            judge_gate_document(&document, &component.scope, std::slice::from_ref(&component))
                .is_ok()
        );
        assert!(matches!(
            judge_gate_document(&document, &component.scope, &[]),
            Err(GateRefusal::InvalidDocument { .. })
        ));
        document.nodes.get_mut("node").unwrap().operation = "removed".to_owned();
        assert!(matches!(
            judge_gate_document(&document, &component.scope, std::slice::from_ref(&component)),
            Err(GateRefusal::InvalidDocument { .. })
        ));
    }

    #[test]
    fn effectful_cases_refuse_but_empty_cases_execute_nothing() {
        let mut component = component();
        component.effects.push(AdmittedComponentEffect {
            package: "wamn:postgres".to_owned(),
            interfaces: vec!["query".to_owned()],
            provenance: ComponentEffectProvenance::Imported,
        });
        let mut document = document();
        assert_eq!(
            judge_gate_document(&document, &component.scope, std::slice::from_ref(&component)),
            Err(GateRefusal::EffectfulComponentReached {
                components: vec!["transform".to_owned()],
            })
        );
        document.cases.clear();
        assert!(
            judge_gate_document(&document, &component.scope, std::slice::from_ref(&component))
                .is_ok()
        );
    }

    #[test]
    fn malformed_cases_refuse_before_component_facts_are_needed() {
        let component = component();
        let mut document = document();
        document.cases.push(document.cases[0].clone());
        assert!(matches!(
            judge_gate_document(&document, &component.scope, &[]),
            Err(GateRefusal::InvalidTestSet { .. })
        ));
    }
}
