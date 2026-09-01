//! Pure semantic gate for a wiring against admitted component-library facts.

use std::collections::BTreeMap;
use std::fmt;

use boon::{Compiler, Draft, Schemas};

use crate::{
    AdmittedComponent, AdmittedComponentOperation, AdmittedComponentPort, ComponentPackageScope,
    ComponentSchema, WiringDocument, WiringNode, schema_digests_match,
};

const PARAMETER_SCHEMA_URI: &str = "mem://wamn-wiring-parameter.json";

/// Stable classification for a refused wiring/component compatibility gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WiringCompatibilityErrorKind {
    FactScopeMismatch,
    MissingComponent,
    IncompatibleInterfaceVersion,
    MissingOperation,
    DuplicateOperationFact,
    UndeclaredParameter,
    MissingRequiredParameter,
    InvalidParameter,
    UnknownOutputPort,
    UnknownInputPort,
    MissingInputPort,
    AmbiguousInputPort,
    SchemaDigestMismatch,
}

/// Contextual refusal from the gate-time wiring compatibility boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WiringCompatibilityError {
    kind: WiringCompatibilityErrorKind,
    detail: Box<str>,
}

impl WiringCompatibilityError {
    /// Stable refusal class for callers that must not match display text.
    pub fn kind(&self) -> WiringCompatibilityErrorKind {
        self.kind
    }

    fn new(kind: WiringCompatibilityErrorKind, detail: impl Into<Box<str>>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for WiringCompatibilityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for WiringCompatibilityError {}

/// Validate one wiring exclusively against facts from its exact package scope.
///
/// This is a publish/gate function, not a delivery-time fallback. Every normal
/// edge resolves the exact authored source and target ports and requires equal
/// canonical JSON-Schema digests. Parameters are both declared and validated
/// against their admitted schemas. No structural subtyping, coercion, default,
/// or cross-version lookup exists.
pub fn validate_wiring_compatibility(
    wiring: &WiringDocument,
    scope: &ComponentPackageScope,
    components: &[AdmittedComponent],
) -> Result<(), WiringCompatibilityError> {
    if let Some(component) = components
        .iter()
        .find(|component| component.scope != *scope)
    {
        return Err(WiringCompatibilityError::new(
            WiringCompatibilityErrorKind::FactScopeMismatch,
            format!(
                "component {:?} fact scope ({:?}, {:?}, {:?}) differs from wiring gate scope ({:?}, {:?}, {:?})",
                component.component,
                component.scope.tenant_id,
                component.scope.package_id,
                component.scope.package_version,
                scope.tenant_id,
                scope.package_id,
                scope.package_version,
            ),
        ));
    }

    let mut resolved = BTreeMap::new();
    for (node_id, node) in &wiring.nodes {
        let operation = resolve_component_operation(node_id, node, components)?;
        validate_parameters(node_id, node, operation.operation)?;
        resolved.insert(node_id.as_str(), operation);
    }

    validate_resolved_edges(wiring, &resolved)
}

/// Validate a wiring against the exact component fact selected for every node.
///
/// Unlike [`validate_wiring_compatibility`], this form admits facts from
/// different exact package scopes. The release publisher owns resolving each
/// authored dependency alias before calling it; this function verifies that
/// the supplied target still matches the node tuple and then applies the same
/// parameter, port, and schema rules as the package-local gate.
pub fn validate_resolved_wiring_compatibility(
    wiring: &WiringDocument,
    components: &BTreeMap<String, AdmittedComponent>,
) -> Result<(), WiringCompatibilityError> {
    let mut resolved = BTreeMap::new();
    for (node_id, node) in &wiring.nodes {
        let component = components.get(node_id).ok_or_else(|| {
            WiringCompatibilityError::new(
                WiringCompatibilityErrorKind::MissingComponent,
                format!("wiring node {node_id:?} has no resolved component fact"),
            )
        })?;
        if component.component != node.component
            || component.interface_version != node.interface_version
        {
            return Err(WiringCompatibilityError::new(
                WiringCompatibilityErrorKind::MissingOperation,
                format!(
                    "wiring node {node_id:?} tuple ({:?}, {:?}) differs from resolved component tuple ({:?}, {:?})",
                    node.component,
                    node.interface_version,
                    component.component,
                    component.interface_version,
                ),
            ));
        }
        let operation = component.operations.get(&node.operation).ok_or_else(|| {
            WiringCompatibilityError::new(
                WiringCompatibilityErrorKind::MissingOperation,
                format!(
                    "wiring node {node_id:?} component {:?} interface {:?} has no operation {:?}",
                    node.component, node.interface_version, node.operation
                ),
            )
        })?;
        validate_parameters(node_id, node, operation)?;
        resolved.insert(node_id.as_str(), ResolvedOperation { operation });
    }
    if components.len() != resolved.len() {
        return Err(WiringCompatibilityError::new(
            WiringCompatibilityErrorKind::DuplicateOperationFact,
            "resolved component facts contain a node absent from the wiring",
        ));
    }

    validate_resolved_edges(wiring, &resolved)
}

fn validate_resolved_edges(
    wiring: &WiringDocument,
    resolved: &BTreeMap<&str, ResolvedOperation<'_>>,
) -> Result<(), WiringCompatibilityError> {
    for edge in &wiring.edges {
        let source = resolved
            .get(edge.from.as_str())
            .expect("WiringDocument validation resolves every source node");
        let target = resolved
            .get(edge.to.as_str())
            .expect("WiringDocument validation resolves every target node");
        let target_port = resolve_input_port(
            &edge.to,
            &wiring.nodes[&edge.to].operation,
            edge.to_port.as_deref(),
            target.operation,
        )?;

        // `error` is router-owned failure data, not a successful component
        // output declaration. The target port still has to exist, but no
        // component output schema can truthfully be compared with it.
        if edge.from_port == wamn_execution_contract::ERROR_PORT {
            continue;
        }
        let Some(source_port) = source
            .operation
            .output_ports
            .iter()
            .find(|port| port.name == edge.from_port)
        else {
            return Err(WiringCompatibilityError::new(
                WiringCompatibilityErrorKind::UnknownOutputPort,
                format!(
                    "wiring node {:?} operation {:?} does not declare output port {:?}",
                    edge.from, wiring.nodes[&edge.from].operation, edge.from_port
                ),
            ));
        };
        if !schema_digests_match(&source_port.schema, &target_port.schema) {
            return Err(WiringCompatibilityError::new(
                WiringCompatibilityErrorKind::SchemaDigestMismatch,
                format!(
                    "wiring edge {:?}.{:?} -> {:?}.{:?} has schema digests {:?} and {:?}",
                    edge.from,
                    edge.from_port,
                    edge.to,
                    target_port.name,
                    source_port.schema.schema_digest,
                    target_port.schema.schema_digest,
                ),
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct ResolvedOperation<'a> {
    operation: &'a AdmittedComponentOperation,
}

fn resolve_component_operation<'a>(
    node_id: &str,
    node: &WiringNode,
    components: &'a [AdmittedComponent],
) -> Result<ResolvedOperation<'a>, WiringCompatibilityError> {
    let component_matches: Vec<_> = components
        .iter()
        .filter(|component| component.component == node.component)
        .collect();
    if component_matches.is_empty() {
        return Err(WiringCompatibilityError::new(
            WiringCompatibilityErrorKind::MissingComponent,
            format!(
                "wiring node {node_id:?} names missing component {:?}",
                node.component
            ),
        ));
    }
    let interface_matches: Vec<_> = component_matches
        .into_iter()
        .filter(|component| component.interface_version == node.interface_version)
        .collect();
    if interface_matches.is_empty() {
        return Err(WiringCompatibilityError::new(
            WiringCompatibilityErrorKind::IncompatibleInterfaceVersion,
            format!(
                "wiring node {node_id:?} component {:?} has no exact interface version {:?}",
                node.component, node.interface_version
            ),
        ));
    }
    let mut operation_matches = interface_matches.into_iter().filter_map(|component| {
        component
            .operations
            .get(&node.operation)
            .map(|operation| ResolvedOperation { operation })
    });
    let Some(component) = operation_matches.next() else {
        return Err(WiringCompatibilityError::new(
            WiringCompatibilityErrorKind::MissingOperation,
            format!(
                "wiring node {node_id:?} component {:?} interface {:?} has no operation {:?}",
                node.component, node.interface_version, node.operation
            ),
        ));
    };
    if operation_matches.next().is_some() {
        return Err(WiringCompatibilityError::new(
            WiringCompatibilityErrorKind::DuplicateOperationFact,
            format!(
                "wiring node {node_id:?} resolves more than one fact for {:?} {:?} {:?}",
                node.component, node.interface_version, node.operation
            ),
        ));
    }
    Ok(component)
}

fn validate_parameters(
    node_id: &str,
    node: &WiringNode,
    operation: &AdmittedComponentOperation,
) -> Result<(), WiringCompatibilityError> {
    for (name, value) in &node.params {
        let Some(parameter) = operation
            .parameters
            .iter()
            .find(|parameter| parameter.name == *name)
        else {
            return Err(WiringCompatibilityError::new(
                WiringCompatibilityErrorKind::UndeclaredParameter,
                format!("wiring node {node_id:?} supplies undeclared parameter {name:?}"),
            ));
        };
        validate_parameter_value(node_id, name, value, &parameter.schema)?;
    }
    if let Some(parameter) = operation
        .parameters
        .iter()
        .find(|parameter| parameter.required && !node.params.contains_key(&parameter.name))
    {
        return Err(WiringCompatibilityError::new(
            WiringCompatibilityErrorKind::MissingRequiredParameter,
            format!(
                "wiring node {node_id:?} omits required parameter {:?}",
                parameter.name
            ),
        ));
    }
    Ok(())
}

fn validate_parameter_value(
    node_id: &str,
    parameter: &str,
    value: &serde_json::Value,
    schema: &ComponentSchema,
) -> Result<(), WiringCompatibilityError> {
    let mut compiler = Compiler::new();
    compiler.set_default_draft(Draft::V2020_12);
    compiler
        .add_resource(PARAMETER_SCHEMA_URI, schema.schema.clone())
        .expect("an admitted component carries a compiled-valid schema");
    let mut schemas = Schemas::new();
    let compiled = compiler
        .compile(PARAMETER_SCHEMA_URI, &mut schemas)
        .expect("an admitted component carries a compiled-valid schema");
    schemas.validate(value, compiled).map_err(|error| {
        WiringCompatibilityError::new(
            WiringCompatibilityErrorKind::InvalidParameter,
            format!(
                "wiring node {node_id:?} parameter {parameter:?} does not match schema digest {:?}: {error}",
                schema.schema_digest
            ),
        )
    })
}

fn resolve_input_port<'a>(
    node_id: &str,
    operation_name: &str,
    authored_port: Option<&str>,
    operation: &'a AdmittedComponentOperation,
) -> Result<&'a AdmittedComponentPort, WiringCompatibilityError> {
    match authored_port {
        Some(name) => operation
            .input_ports
            .iter()
            .find(|port| port.name == name)
            .ok_or_else(|| {
                WiringCompatibilityError::new(
                    WiringCompatibilityErrorKind::UnknownInputPort,
                    format!(
                        "wiring target {node_id:?} operation {:?} does not declare input port {name:?}",
                        operation_name
                    ),
                )
            }),
        None if operation.input_ports.len() == 1 => Ok(&operation.input_ports[0]),
        None if operation.input_ports.is_empty() => Err(WiringCompatibilityError::new(
            WiringCompatibilityErrorKind::MissingInputPort,
            format!(
                "wiring target {node_id:?} operation {:?} declares no input port",
                operation_name
            ),
        )),
        None => Err(WiringCompatibilityError::new(
            WiringCompatibilityErrorKind::AmbiguousInputPort,
            format!(
                "wiring target {node_id:?} operation {:?} declares {} input ports; to-port is required",
                operation_name,
                operation.input_ports.len()
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{AdmittedComponentParameter, WiringEdge, WiringTerminal};

    fn schema(value: serde_json::Value) -> ComponentSchema {
        ComponentSchema {
            schema_digest: wamn_execution_contract::canonical_json_sha256(&value),
            schema: value,
        }
    }

    fn component(
        name: &str,
        operation: &str,
        input_name: &str,
        input_schema: serde_json::Value,
        output_name: &str,
        output_schema: serde_json::Value,
    ) -> AdmittedComponent {
        AdmittedComponent {
            scope: scope(),
            component: name.to_string(),
            interface_version: "0.1.0".to_string(),
            operations: BTreeMap::from([(
                operation.to_string(),
                AdmittedComponentOperation {
                    registered_operation: None,
                    input_ports: vec![AdmittedComponentPort {
                        name: input_name.to_string(),
                        schema: schema(input_schema),
                    }],
                    output_ports: vec![AdmittedComponentPort {
                        name: output_name.to_string(),
                        schema: schema(output_schema),
                    }],
                    parameters: Vec::new(),
                },
            )]),
            component_digest: format!("sha256:{}", "a".repeat(64)),
            imports: Vec::new(),
            imports_fingerprint: format!("sha256:{}", "b".repeat(64)),
            effects: Vec::new(),
        }
    }

    fn operation_mut<'a>(
        component: &'a mut AdmittedComponent,
        operation: &str,
    ) -> &'a mut AdmittedComponentOperation {
        component
            .operations
            .get_mut(operation)
            .expect("fixture operation exists")
    }

    fn scope() -> ComponentPackageScope {
        ComponentPackageScope {
            tenant_id: "t1".to_string(),
            package_id: "app".to_string(),
            package_version: "1.0.0".to_string(),
        }
    }

    fn wiring() -> WiringDocument {
        WiringDocument::new(
            "orders",
            1,
            "source",
            BTreeMap::from([
                (
                    "source".to_string(),
                    WiringNode {
                        component: "source".to_string(),
                        interface_version: "0.1.0".to_string(),
                        operation: "read".to_string(),
                        operation_dependency: None,
                        params: BTreeMap::new(),
                        terminal: None,
                    },
                ),
                (
                    "target".to_string(),
                    WiringNode {
                        component: "target".to_string(),
                        interface_version: "0.1.0".to_string(),
                        operation: "write".to_string(),
                        operation_dependency: None,
                        params: BTreeMap::from([("relation".to_string(), json!("orders"))]),
                        terminal: Some(WiringTerminal::Respond),
                    },
                ),
            ]),
            vec![WiringEdge {
                from: "source".to_string(),
                from_port: "record".to_string(),
                to: "target".to_string(),
                to_port: Some("record".to_string()),
            }],
            Vec::new(),
        )
        .expect("fixture wiring is structurally valid")
    }

    fn components() -> Vec<AdmittedComponent> {
        let record = json!({"type": "object", "required": ["id"]});
        let mut source = component(
            "source",
            "read",
            "request",
            json!({}),
            "record",
            record.clone(),
        );
        operation_mut(&mut source, "read").input_ports.clear();
        let mut target = component("target", "write", "record", record, "main", json!({}));
        operation_mut(&mut target, "write").parameters = vec![AdmittedComponentParameter {
            name: "relation".to_string(),
            schema: schema(json!({"type": "string"})),
            required: true,
        }];
        vec![source, target]
    }

    #[test]
    fn exact_scoped_schema_and_parameter_facts_admit() {
        validate_wiring_compatibility(&wiring(), &scope(), &components())
            .expect("exact compatibility admits");
    }

    #[test]
    fn resolved_cross_package_facts_keep_their_exact_target_scopes() {
        let mut facts = components();
        facts[1].scope.package_id = "base".to_owned();
        facts[1].scope.package_version = "3.1.0".to_owned();
        let resolved = BTreeMap::from([
            ("source".to_owned(), facts[0].clone()),
            ("target".to_owned(), facts[1].clone()),
        ]);

        validate_resolved_wiring_compatibility(&wiring(), &resolved)
            .expect("exact per-node target facts admit");

        assert_eq!(resolved["source"].scope, scope());
        assert_eq!(resolved["target"].scope.package_id, "base");
        assert_eq!(resolved["target"].scope.package_version, "3.1.0");
    }

    #[test]
    fn either_endpoint_digest_or_port_drift_refuses() {
        let mut source_digest = components();
        operation_mut(&mut source_digest[0], "read").output_ports[0].schema =
            schema(json!({"type": "string"}));
        assert_eq!(
            validate_wiring_compatibility(&wiring(), &scope(), &source_digest)
                .unwrap_err()
                .kind(),
            WiringCompatibilityErrorKind::SchemaDigestMismatch
        );

        let mut target_digest = components();
        operation_mut(&mut target_digest[1], "write").input_ports[0].schema =
            schema(json!({"type": "array"}));
        assert_eq!(
            validate_wiring_compatibility(&wiring(), &scope(), &target_digest)
                .unwrap_err()
                .kind(),
            WiringCompatibilityErrorKind::SchemaDigestMismatch
        );

        let mut wrong_port = wiring();
        wrong_port.edges[0].to_port = Some("missing".to_string());
        assert_eq!(
            validate_wiring_compatibility(&wrong_port, &scope(), &components())
                .unwrap_err()
                .kind(),
            WiringCompatibilityErrorKind::UnknownInputPort
        );
    }

    #[test]
    fn stale_scope_and_invalid_parameter_refuse_before_publish() {
        let mut stale = components();
        stale[0].scope.package_version = "2.0.0".to_string();
        assert_eq!(
            validate_wiring_compatibility(&wiring(), &scope(), &stale)
                .unwrap_err()
                .kind(),
            WiringCompatibilityErrorKind::FactScopeMismatch
        );

        let mut invalid = wiring();
        invalid.nodes.get_mut("target").unwrap().params =
            BTreeMap::from([("relation".to_string(), json!(42))]);
        assert_eq!(
            validate_wiring_compatibility(&invalid, &scope(), &components())
                .unwrap_err()
                .kind(),
            WiringCompatibilityErrorKind::InvalidParameter
        );

        let mut missing = wiring();
        missing.nodes.get_mut("target").unwrap().params.clear();
        assert_eq!(
            validate_wiring_compatibility(&missing, &scope(), &components())
                .unwrap_err()
                .kind(),
            WiringCompatibilityErrorKind::MissingRequiredParameter
        );
    }

    #[test]
    fn equality_has_no_structural_subtyping_fallback() {
        let mut structurally_wider = components();
        operation_mut(&mut structurally_wider[1], "write").input_ports[0].schema = schema(json!({
            "type": "object",
            "required": ["id"],
            "additionalProperties": true
        }));
        assert_eq!(
            validate_wiring_compatibility(&wiring(), &scope(), &structurally_wider)
                .unwrap_err()
                .kind(),
            WiringCompatibilityErrorKind::SchemaDigestMismatch
        );
    }

    #[test]
    fn error_edges_validate_the_target_without_fabricating_a_component_output_schema() {
        let mut error = wiring();
        error.edges[0].from_port = wamn_execution_contract::ERROR_PORT.to_string();
        validate_wiring_compatibility(&error, &scope(), &components())
            .expect("the router-owned error payload has no component output schema");

        error.edges[0].to_port = Some("missing".to_string());
        assert_eq!(
            validate_wiring_compatibility(&error, &scope(), &components())
                .unwrap_err()
                .kind(),
            WiringCompatibilityErrorKind::UnknownInputPort
        );
    }
}
