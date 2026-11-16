//! Pure semantic condition for a wiring against admitted component facts.

use crate::canonical_name::CanonicalName;

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;

use boon::{Compiler, Draft, Schemas};

use crate::{
    AdmittedComponent, AdmittedComponentOperation, AdmittedComponentPort,
    ComponentPackageCoordinate, ComponentSchema, WiringDocument, WiringNode,
};

const PARAMETER_SCHEMA_URI: &str = "mem://wamn-wiring-parameter.json";

/// Stable classification for a refused wiring/component compatibility condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WiringCompatibilityErrorType {
    FactTargetMismatch,
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
    SchemaMismatch,
    JsonataSample,
}

/// Contextual refusal from the condition-time wiring compatibility boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WiringCompatibilityError {
    type_: WiringCompatibilityErrorType,
    detail: Box<str>,
}

impl WiringCompatibilityError {
    /// Stable refusal class for callers that must not match display text.
    pub fn error_type(&self) -> WiringCompatibilityErrorType {
        self.type_
    }

    pub(super) fn new(type_: WiringCompatibilityErrorType, detail: impl Into<Box<str>>) -> Self {
        Self {
            type_,
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

/// Validate one wiring exclusively against facts from its exact package target.
///
/// This is a publish/condition function, not a delivery-time fallback. Every normal
/// edge resolves the exact authored source and target ports and requires equal
/// JSON Schemas. Parameters are both declared and validated
/// against their admitted schemas. No structural subtyping, coercion, default,
/// or cross-version lookup exists.
pub fn validate_wiring_compatibility(
    wiring: &WiringDocument,
    target: &ComponentPackageCoordinate,
    components: &[AdmittedComponent],
) -> Result<(), WiringCompatibilityError> {
    if let Some(component) = components
        .iter()
        .find(|component| component.target != *target)
    {
        return Err(WiringCompatibilityError::new(
            WiringCompatibilityErrorType::FactTargetMismatch,
            format!(
                "component {:?} fact target ({:?}, {:?}, {:?}) differs from wiring condition target ({:?}, {:?}, {:?})",
                component.component,
                component.target.project_env_key,
                component.target.package_id,
                component.target.package_version,
                target.project_env_key,
                target.package_id,
                target.package_version,
            ),
        ));
    }

    let mut resolved = BTreeMap::new();
    for (node_id, node) in &wiring.nodes {
        let (component, operation) = resolve_component_operation(node_id, node, components)?;
        validate_parameters(node_id, node, operation)?;
        resolved.insert(
            node_id,
            crate::specialize_wiring_operation(node_id, node, component, operation)?,
        );
    }

    validate_resolved_edges(wiring, &resolved)
}

/// Validate a wiring against the exact component fact selected for every node.
///
/// Unlike [`validate_wiring_compatibility`], this form admits facts from
/// different exact package targets. The release publisher owns resolving each
/// authored dependency alias before calling it; this function verifies that
/// the supplied target still matches the node tuple and then applies the same
/// parameter, port, and schema rules as the package-local condition.
pub fn validate_resolved_wiring_compatibility(
    wiring: &WiringDocument,
    components: &BTreeMap<CanonicalName, AdmittedComponent>,
) -> Result<(), WiringCompatibilityError> {
    let mut resolved = BTreeMap::new();
    for (node_id, node) in &wiring.nodes {
        let component = components.get(node_id).ok_or_else(|| {
            WiringCompatibilityError::new(
                WiringCompatibilityErrorType::MissingComponent,
                format!("wiring node {node_id:?} has no resolved component fact"),
            )
        })?;
        if component.component != node.component
            || component.interface_version != node.interface_version
        {
            return Err(WiringCompatibilityError::new(
                WiringCompatibilityErrorType::MissingOperation,
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
                WiringCompatibilityErrorType::MissingOperation,
                format!(
                    "wiring node {node_id:?} component {:?} interface {:?} has no operation {:?}",
                    node.component, node.interface_version, node.operation
                ),
            )
        })?;
        validate_parameters(node_id, node, operation)?;
        resolved.insert(
            node_id,
            crate::specialize_wiring_operation(node_id, node, component, operation)?,
        );
    }
    if components.len() != resolved.len() {
        return Err(WiringCompatibilityError::new(
            WiringCompatibilityErrorType::DuplicateOperationFact,
            "resolved component facts contain a node absent from the wiring",
        ));
    }

    validate_resolved_edges(wiring, &resolved)
}

fn validate_resolved_edges(
    wiring: &WiringDocument,
    resolved: &BTreeMap<&CanonicalName, Cow<'_, AdmittedComponentOperation>>,
) -> Result<(), WiringCompatibilityError> {
    if let Some(node_id) = wiring
        .response
        .as_ref()
        .and_then(|response| response.committed_result.as_ref())
    {
        let operation = resolved
            .get(node_id)
            .ok_or_else(|| {
                WiringCompatibilityError::new(
                    WiringCompatibilityErrorType::MissingOperation,
                    format!("committed-result node {node_id:?} has no resolved operation"),
                )
            })?
            .as_ref();
        if operation.committed_result_schema.is_none() {
            return Err(WiringCompatibilityError::new(
                WiringCompatibilityErrorType::MissingOperation,
                format!(
                    "committed-result node {node_id:?} has no admitted committed result schema"
                ),
            ));
        }
        crate::component_facts::validate_committed_result_schema(
            operation.registered_operation.as_ref(),
            operation.committed_result_schema.as_ref(),
        )
        .map_err(|error| {
            WiringCompatibilityError::new(
                WiringCompatibilityErrorType::MissingOperation,
                format!("committed-result node {node_id:?} has invalid admitted facts: {error}"),
            )
        })?;
    }
    for edge in &wiring.edges {
        let source = resolved
            .get(&edge.from)
            .expect("WiringDocument validation resolves every source node");
        let target = resolved
            .get(&edge.to)
            .expect("WiringDocument validation resolves every target node");
        let target_port = resolve_input_port(
            &edge.to,
            &wiring.nodes[&edge.to].operation,
            edge.to_port.as_ref(),
            target,
        )?;

        // `error` is router-owned failure data, not a successful component
        // output declaration. The target port still has to exist, but no
        // component output schema can truthfully be compared with it.
        if edge.from_port == crate::ERROR_PORT {
            continue;
        }
        let Some(source_port) = source
            .output_ports
            .iter()
            .find(|port| port.name == edge.from_port)
        else {
            return Err(WiringCompatibilityError::new(
                WiringCompatibilityErrorType::UnknownOutputPort,
                format!(
                    "wiring node {:?} operation {:?} does not declare output port {:?}",
                    edge.from, wiring.nodes[&edge.from].operation, edge.from_port
                ),
            ));
        };
        if source_port.schema != target_port.schema {
            return Err(WiringCompatibilityError::new(
                WiringCompatibilityErrorType::SchemaMismatch,
                format!(
                    "wiring edge {}.{} -> {}.{} has schemas {} and {}",
                    edge.from,
                    edge.from_port,
                    edge.to,
                    target_port.name,
                    source_port.schema.schema,
                    target_port.schema.schema,
                ),
            ));
        }
    }
    for (node_id, resolved) in resolved {
        if matches!(resolved, Cow::Owned(_)) {
            crate::wiring_jsonata::validate_samples(node_id, &wiring.nodes[*node_id])?;
        }
    }
    Ok(())
}

fn resolve_component_operation<'a>(
    node_id: &CanonicalName,
    node: &WiringNode,
    components: &'a [AdmittedComponent],
) -> Result<(&'a AdmittedComponent, &'a AdmittedComponentOperation), WiringCompatibilityError> {
    let component_matches: Vec<_> = components
        .iter()
        .filter(|component| component.component == node.component)
        .collect();
    if component_matches.is_empty() {
        return Err(WiringCompatibilityError::new(
            WiringCompatibilityErrorType::MissingComponent,
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
            WiringCompatibilityErrorType::IncompatibleInterfaceVersion,
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
            .map(|operation| (component, operation))
    });
    let Some(component) = operation_matches.next() else {
        return Err(WiringCompatibilityError::new(
            WiringCompatibilityErrorType::MissingOperation,
            format!(
                "wiring node {node_id:?} component {:?} interface {:?} has no operation {:?}",
                node.component, node.interface_version, node.operation
            ),
        ));
    };
    if operation_matches.next().is_some() {
        return Err(WiringCompatibilityError::new(
            WiringCompatibilityErrorType::DuplicateOperationFact,
            format!(
                "wiring node {node_id:?} resolves more than one fact for {:?} {:?} {:?}",
                node.component, node.interface_version, node.operation
            ),
        ));
    }
    Ok(component)
}

fn validate_parameters(
    node_id: &CanonicalName,
    node: &WiringNode,
    operation: &AdmittedComponentOperation,
) -> Result<(), WiringCompatibilityError> {
    for (name, value) in &node.params {
        if matches!(name.as_str(), "retry" | "deadline-ms") {
            continue;
        }
        let Some(parameter) = operation
            .parameters
            .iter()
            .find(|parameter| parameter.name == *name)
        else {
            return Err(WiringCompatibilityError::new(
                WiringCompatibilityErrorType::UndeclaredParameter,
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
            WiringCompatibilityErrorType::MissingRequiredParameter,
            format!(
                "wiring node {node_id} omits required parameter {}",
                parameter.name
            ),
        ));
    }
    Ok(())
}

fn validate_parameter_value(
    node_id: &CanonicalName,
    parameter: &CanonicalName,
    value: &serde_json::Value,
    schema: &ComponentSchema,
) -> Result<(), WiringCompatibilityError> {
    let mut compiler = Compiler::new();
    compiler.set_default_draft(Draft::V2020_12);
    compiler
        .add_resource(PARAMETER_SCHEMA_URI, schema.schema.clone())
        .expect("an admitted component carries a compiled-valid schema");
    let mut schemas = Schemas::new();
    let schema_index = compiler
        .compile(PARAMETER_SCHEMA_URI, &mut schemas)
        .expect("an admitted component carries a compiled-valid schema");
    schemas.validate(value, schema_index).map_err(|error| {
        WiringCompatibilityError::new(
            WiringCompatibilityErrorType::InvalidParameter,
            format!(
                "wiring node {node_id:?} parameter {parameter:?} does not match schema {}: {error}",
                schema.schema
            ),
        )
    })
}

/// Resolve an authored input port, or the operation's single input.
pub fn resolve_input_port<'a>(
    node_id: &CanonicalName,
    operation_name: &CanonicalName,
    authored_port: Option<&CanonicalName>,
    operation: &'a AdmittedComponentOperation,
) -> Result<&'a AdmittedComponentPort, WiringCompatibilityError> {
    match authored_port {
        Some(name) => operation
            .input_ports
            .iter()
            .find(|port| &port.name == name)
            .ok_or_else(|| {
                WiringCompatibilityError::new(
                    WiringCompatibilityErrorType::UnknownInputPort,
                    format!(
                        "wiring target {node_id:?} operation {operation_name:?} does not declare input port {name:?}"
                    ),
                )
            }),
        None if operation.input_ports.len() == 1 => Ok(&operation.input_ports[0]),
        None if operation.input_ports.is_empty() => Err(WiringCompatibilityError::new(
            WiringCompatibilityErrorType::MissingInputPort,
            format!("wiring target {node_id:?} operation {operation_name:?} declares no input port"),
        )),
        None => Err(WiringCompatibilityError::new(
            WiringCompatibilityErrorType::AmbiguousInputPort,
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
        ComponentSchema { schema: value }
    }

    fn component(
        name: &str,
        operation: &CanonicalName,
        input_name: &str,
        input_schema: serde_json::Value,
        output_name: &str,
        output_schema: serde_json::Value,
    ) -> AdmittedComponent {
        AdmittedComponent {
            target: target(),
            component: name.parse().unwrap(),
            interface_version: "0.1.0".parse().unwrap(),
            operations: BTreeMap::from([(
                operation.clone(),
                AdmittedComponentOperation {
                    committed_result_schema: None,
                    registered_operation: None,
                    host_transaction: false,
                    input_ports: vec![AdmittedComponentPort {
                        name: input_name.parse().unwrap(),
                        schema: schema(input_schema),
                    }],
                    output_ports: vec![AdmittedComponentPort {
                        name: output_name.parse().unwrap(),
                        schema: schema(output_schema),
                    }],
                    parameters: Vec::new(),
                },
            )]),
            component_digest: (format!("sha256:{}", "a".repeat(64)))
                .parse::<wamn_digest::Digest>()
                .unwrap(),
            imports: Vec::new(),
            imports_fingerprint: (format!("sha256:{}", "b".repeat(64)))
                .parse::<wamn_digest::Digest>()
                .unwrap(),
            effects: Vec::new(),
        }
    }

    fn operation_mut<'a>(
        component: &'a mut AdmittedComponent,
        operation: &CanonicalName,
    ) -> &'a mut AdmittedComponentOperation {
        component
            .operations
            .get_mut(operation)
            .expect("fixture operation exists")
    }

    fn target() -> ComponentPackageCoordinate {
        ComponentPackageCoordinate {
            project_env_key: "t1--dev--project".parse().unwrap(),
            package_id: "app".parse().unwrap(),
            package_version: "1.0.0".parse().unwrap(),
        }
    }

    fn wiring() -> WiringDocument {
        WiringDocument::new(
            "orders".parse().unwrap(),
            1,
            "source".parse().unwrap(),
            BTreeMap::from([
                (
                    "source".parse().unwrap(),
                    WiringNode {
                        component: "source".parse().unwrap(),
                        interface_version: "0.1.0".parse().unwrap(),
                        operation: "read".parse().unwrap(),
                        operation_dependency: None,
                        params: BTreeMap::new(),
                        terminal: None,
                    },
                ),
                (
                    "target".parse().unwrap(),
                    WiringNode {
                        component: "target".parse().unwrap(),
                        interface_version: "0.1.0".parse().unwrap(),
                        operation: "write".parse().unwrap(),
                        operation_dependency: None,
                        params: BTreeMap::from([("relation".parse().unwrap(), json!("orders"))]),
                        terminal: Some(WiringTerminal::Respond),
                    },
                ),
            ]),
            vec![WiringEdge {
                from: "source".parse().unwrap(),
                from_port: "record".parse().unwrap(),
                to: "target".parse().unwrap(),
                to_port: Some("record".parse().unwrap()),
            }],
        )
        .expect("fixture wiring is structurally valid")
    }

    fn components() -> Vec<AdmittedComponent> {
        let record = json!({"type": "object", "required": ["id"]});
        let mut source = component(
            "source",
            &"read".parse().unwrap(),
            "request",
            json!({}),
            "record",
            record.clone(),
        );
        operation_mut(&mut source, &"read".parse().unwrap())
            .input_ports
            .clear();
        let mut target = component(
            "target",
            &"write".parse().unwrap(),
            "record",
            record,
            "main",
            json!({}),
        );
        operation_mut(&mut target, &"write".parse().unwrap()).parameters =
            vec![AdmittedComponentParameter {
                name: "relation".parse().unwrap(),
                schema: schema(json!({"type": "string"})),
                required: true,
            }];
        vec![source, target]
    }

    #[test]
    fn response_commit_selection_requires_the_selected_admitted_promise() {
        let mut wiring = wiring();
        wiring.response = Some(crate::WiringResponse {
            node: "target".parse().unwrap(),
            schema: json!({"type": "array"}),
            committed_result: Some("source".parse().unwrap()),
        });
        let mut facts = components();
        assert!(validate_wiring_compatibility(&wiring, &target(), &facts).is_err());
        let operation = operation_mut(&mut facts[0], &"read".parse().unwrap());
        operation.committed_result_schema = Some(schema(json!({"type": "object"})));
        assert!(validate_wiring_compatibility(&wiring, &target(), &facts).is_err());
        let registered = "app:order/read@1.0.0";
        let mut operation = facts[0].operations.remove("read").unwrap();
        operation.registered_operation = Some(registered.parse().unwrap());
        facts[0]
            .operations
            .insert(registered.parse().unwrap(), operation);
        wiring.nodes.get_mut("source").unwrap().operation = registered.parse().unwrap();
        validate_wiring_compatibility(&wiring, &target(), &facts)
            .expect("response and edge schemas remain separate");
        let resolved = BTreeMap::from([
            ("source".parse().unwrap(), facts[0].clone()),
            ("target".parse().unwrap(), facts[1].clone()),
        ]);
        validate_resolved_wiring_compatibility(&wiring, &resolved)
            .expect("release resolution enforces the same admitted promise");
        wiring.response.as_mut().unwrap().committed_result = Some("target".parse().unwrap());
        assert!(validate_resolved_wiring_compatibility(&wiring, &resolved).is_err());
    }

    #[test]
    fn exact_targeted_schema_and_parameter_facts_admit() {
        validate_wiring_compatibility(&wiring(), &target(), &components())
            .expect("exact compatibility admits");
    }

    #[test]
    fn resolved_cross_package_facts_keep_their_exact_target_targets() {
        let mut facts = components();
        facts[1].target.package_id = "base".parse().unwrap();
        facts[1].target.package_version = "3.1.0".parse().unwrap();
        let resolved = BTreeMap::from([
            ("source".parse().unwrap(), facts[0].clone()),
            ("target".parse().unwrap(), facts[1].clone()),
        ]);

        validate_resolved_wiring_compatibility(&wiring(), &resolved)
            .expect("exact per-node target facts admit");

        assert_eq!(resolved["source"].target, target());
        assert_eq!(
            resolved["target"].target.package_id,
            "base".parse().unwrap()
        );
        assert_eq!(
            resolved["target"].target.package_version,
            "3.1.0".parse().unwrap()
        );
    }

    #[test]
    fn either_endpoint_digest_or_port_drift_refuses() {
        let mut source_digest = components();
        operation_mut(&mut source_digest[0], &"read".parse().unwrap()).output_ports[0].schema =
            schema(json!({"type": "string"}));
        assert_eq!(
            validate_wiring_compatibility(&wiring(), &target(), &source_digest)
                .unwrap_err()
                .error_type(),
            WiringCompatibilityErrorType::SchemaMismatch
        );

        let mut target_digest = components();
        operation_mut(&mut target_digest[1], &"write".parse().unwrap()).input_ports[0].schema =
            schema(json!({"type": "array"}));
        assert_eq!(
            validate_wiring_compatibility(&wiring(), &target(), &target_digest)
                .unwrap_err()
                .error_type(),
            WiringCompatibilityErrorType::SchemaMismatch
        );

        let mut wrong_port = wiring();
        wrong_port.edges[0].to_port = Some("missing".parse().unwrap());
        assert_eq!(
            validate_wiring_compatibility(&wrong_port, &target(), &components())
                .unwrap_err()
                .error_type(),
            WiringCompatibilityErrorType::UnknownInputPort
        );
    }

    #[test]
    fn stale_target_and_invalid_parameter_refuse_before_publish() {
        let mut stale = components();
        stale[0].target.package_version = "2.0.0".parse().unwrap();
        assert_eq!(
            validate_wiring_compatibility(&wiring(), &target(), &stale)
                .unwrap_err()
                .error_type(),
            WiringCompatibilityErrorType::FactTargetMismatch
        );

        let mut invalid = wiring();
        invalid.nodes.get_mut("target").unwrap().params =
            BTreeMap::from([("relation".parse().unwrap(), json!(42))]);
        assert_eq!(
            validate_wiring_compatibility(&invalid, &target(), &components())
                .unwrap_err()
                .error_type(),
            WiringCompatibilityErrorType::InvalidParameter
        );

        let mut missing = wiring();
        missing.nodes.get_mut("target").unwrap().params.clear();
        assert_eq!(
            validate_wiring_compatibility(&missing, &target(), &components())
                .unwrap_err()
                .error_type(),
            WiringCompatibilityErrorType::MissingRequiredParameter
        );
    }

    #[test]
    fn equality_has_no_structural_subtyping_fallback() {
        let mut structurally_wider = components();
        operation_mut(&mut structurally_wider[1], &"write".parse().unwrap()).input_ports[0]
            .schema = schema(json!({
            "type": "object",
            "required": ["id"],
            "additionalProperties": true
        }));
        assert_eq!(
            validate_wiring_compatibility(&wiring(), &target(), &structurally_wider)
                .unwrap_err()
                .error_type(),
            WiringCompatibilityErrorType::SchemaMismatch
        );
    }

    #[test]
    fn error_edges_validate_the_target_without_fabricating_a_component_output_schema() {
        let mut error = wiring();
        error.edges[0].from_port = crate::ERROR_PORT.parse().unwrap();
        validate_wiring_compatibility(&error, &target(), &components())
            .expect("the router-owned error payload has no component output schema");

        error.edges[0].to_port = Some("missing".parse().unwrap());
        assert_eq!(
            validate_wiring_compatibility(&error, &target(), &components())
                .unwrap_err()
                .error_type(),
            WiringCompatibilityErrorType::UnknownInputPort
        );
    }
    #[test]
    fn publication_allows_sample_errors_but_refuses_an_output_outside_the_schema() {
        let export = "wamn:node/handler@0.1.0".parse().unwrap();
        let mut jsonata = component("jsonata", &export, "input", json!({}), "main", json!({}));
        jsonata.target = crate::palette_target(&jsonata.target.project_env_key);
        operation_mut(&mut jsonata, &export).parameters =
            ["expression", "input-schema", "output-schema"]
                .into_iter()
                .map(|name| AdmittedComponentParameter {
                    name: name.parse().unwrap(),
                    schema: schema(if name == "expression" {
                        json!({"type": "string"})
                    } else {
                        json!({})
                    }),
                    required: true,
                })
                .collect();
        let mut document = WiringDocument::parse(&json!({
            "format-version": "0.1", "wiring-id": "sample-errors", "version": 1, "entry": "map",
            "nodes": {"map": {"component": "jsonata", "interface-version": "0.1.0",
                "operation": "wamn:node/handler@0.1.0", "params": {
                    "input-schema": {"type": "boolean"},
                    "expression": "($assert($); 42)", "output-schema": {"type": "integer"}
                }}}, "edges": []
        }))
        .unwrap();
        let components = BTreeMap::from([("map".parse().unwrap(), jsonata)]);
        validate_resolved_wiring_compatibility(&document, &components).unwrap();
        document.nodes.get_mut("map").unwrap().params.insert(
            "expression".parse().unwrap(),
            json!("($assert($); \"text\")"),
        );
        assert_eq!(
            validate_resolved_wiring_compatibility(&document, &components)
                .unwrap_err()
                .error_type(),
            WiringCompatibilityErrorType::JsonataSample
        );
    }

    #[test]
    fn publication_checks_the_declared_jsonata_input_and_output_schemas() {
        let mut jsonata = component(
            "jsonata",
            &"wamn:node/handler@0.1.0".parse().unwrap(),
            "input",
            json!({}),
            "main",
            json!({}),
        );
        jsonata.target = crate::palette_target(&jsonata.target.project_env_key);
        operation_mut(&mut jsonata, &"wamn:node/handler@0.1.0".parse().unwrap())
            .input_ports
            .push(AdmittedComponentPort {
                name: "items".parse().unwrap(),
                schema: schema(json!({"type": "array"})),
            });
        operation_mut(&mut jsonata, &"wamn:node/handler@0.1.0".parse().unwrap()).parameters = vec![
            AdmittedComponentParameter {
                name: "expression".parse().unwrap(),
                schema: schema(json!({"type": "string"})),
                required: true,
            },
            AdmittedComponentParameter {
                name: "input-schema".parse().unwrap(),
                schema: schema(json!({})),
                required: true,
            },
            AdmittedComponentParameter {
                name: "output-schema".parse().unwrap(),
                schema: schema(json!({})),
                required: true,
            },
        ];
        let mut document = WiringDocument::parse(&json!({
            "format-version": "0.1", "wiring-id": "typed", "version": 1, "entry": "source",
            "nodes": {"source": {"component": "source", "interface-version": "0.1.0", "operation": "read"}, "map": {"component": "jsonata", "interface-version": "0.1.0",
                "operation": "wamn:node/handler@0.1.0", "params": {
                    "input-schema": {"type": "object", "required": ["flag"], "properties": {"flag": {"type": "boolean"}}},
                    "expression": "flag ? 42 : 43", "output-schema": {"type": "integer"}
                }}, "consume": {"component": "sink", "interface-version": "0.1.0", "operation": "consume"}},
            "edges": [{"from": "source", "to": "map", "to-port": "input"}, {"from": "map", "to": "consume"}]
        }))
        .unwrap();
        let sink = component(
            "sink",
            &"consume".parse().unwrap(),
            "input",
            json!({"type": "integer"}),
            "main",
            json!({}),
        );
        let source = component(
            "source",
            &"read".parse().unwrap(),
            "input",
            json!({}),
            "main",
            document.nodes["map"].params["input-schema"].clone(),
        );
        let components = BTreeMap::from([
            ("source".parse().unwrap(), source),
            ("map".parse().unwrap(), jsonata),
            ("consume".parse().unwrap(), sink),
        ]);
        for port in ["input", "items"] {
            document.edges[0].to_port = Some(port.parse().unwrap());
            validate_resolved_wiring_compatibility(&document, &components)
                .expect("both JSONata inputs use the declared schema");
        }
        let input = document
            .nodes
            .get_mut("map")
            .unwrap()
            .params
            .insert("input-schema".parse().unwrap(), json!({"type": "string"}))
            .unwrap();
        assert_eq!(
            validate_resolved_wiring_compatibility(&document, &components)
                .unwrap_err()
                .error_type(),
            WiringCompatibilityErrorType::SchemaMismatch
        );
        document
            .nodes
            .get_mut("map")
            .unwrap()
            .params
            .insert("input-schema".parse().unwrap(), input);
        document.nodes.get_mut("map").unwrap().params.insert(
            "expression".parse().unwrap(),
            json!("flag ? 42 : \"wrong\""),
        );
        let error = validate_resolved_wiring_compatibility(&document, &components).unwrap_err();
        assert_eq!(
            error.error_type(),
            WiringCompatibilityErrorType::JsonataSample
        );
    }
}
