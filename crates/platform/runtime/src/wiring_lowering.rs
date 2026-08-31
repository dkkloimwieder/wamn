//! Pure lowering from a package-owned wiring to the router's executable graph.
//!
//! The package owns authored wiring data and the router owns a storage-blind
//! walk. This module is the sole consumer boundary between them: callers supply
//! one already-active wiring and the operation facts gated with it, and this
//! module refuses any one-sided drift before constructing a [`Wiring`]. It does
//! not read storage or define how the future component library persists facts;
//! that library projects its component, interface, operation, digest, port and
//! parameter declarations onto [`WiringOperationFact`].

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};
use wamn_catalog::{
    WiringDocument, WiringNode as CatalogWiringNode, WiringTerminal as CatalogTerminal,
};
use wamn_router::{
    ERROR_PORT, Terminal, Wiring, WiringEdge as RouterEdge, WiringError, WiringNode as RouterNode,
};

/// Tenant/package/environment identity shared by a wiring and its admitted facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WiringScope<'a> {
    pub tenant_id: &'a str,
    pub package_id: &'a str,
    pub environment: &'a str,
}

/// One active wiring row after exact-release membership and pointer resolution.
#[derive(Debug, Clone, Copy)]
pub struct GatedActiveWiring<'a> {
    pub scope: WiringScope<'a>,
    pub package_version: &'a str,
    pub document: &'a WiringDocument,
}

/// One declared operation parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WiringParameterFact {
    pub required: bool,
}

/// The component-library facts needed to lower one node occurrence.
///
/// This is deliberately a consumer projection, not a persistence contract.
/// `component_digest` becomes the router's instance-pool key; the logical name,
/// interface version and operation select one admitted digest before the
/// executable graph is built. The digest then identifies the uniform handler;
/// operation is not carried into the router call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WiringOperationFact {
    pub component: String,
    pub interface_version: String,
    pub operation: String,
    pub component_digest: String,
    pub input_ports: BTreeSet<String>,
    pub output_ports: BTreeSet<String>,
    pub parameters: BTreeMap<String, WiringParameterFact>,
}

/// Operation facts admitted for one exact package version in one environment.
#[derive(Debug, Clone, Copy)]
pub struct ScopedWiringOperationFacts<'a> {
    pub scope: WiringScope<'a>,
    pub package_version: &'a str,
    pub operations: &'a [WiringOperationFact],
}

/// Project one persisted component-library row onto the lowering seam.
///
/// Every component-owned field survives exactly. Environment is intentionally
/// supplied by [`ScopedWiringOperationFacts`] because it belongs to the active
/// wiring, not component admission. Typed schemas remain available on the
/// source fact for the semantic gate and are erased only after that gate has
/// established exact digest compatibility.
pub fn project_component_operation(
    component: &wamn_catalog::AdmittedComponent,
) -> WiringOperationFact {
    WiringOperationFact {
        component: component.component.clone(),
        interface_version: component.interface_version.clone(),
        operation: component.operation.clone(),
        component_digest: component.component_digest.clone(),
        input_ports: component
            .input_ports
            .iter()
            .map(|port| port.name.clone())
            .collect(),
        output_ports: component
            .output_ports
            .iter()
            .map(|port| port.name.clone())
            .collect(),
        parameters: component
            .parameters
            .iter()
            .map(|parameter| {
                (
                    parameter.name.clone(),
                    WiringParameterFact {
                        required: parameter.required,
                    },
                )
            })
            .collect(),
    }
}

/// Stable classification for a refused catalog-to-router lowering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WiringLoweringErrorKind {
    ScopeMismatch,
    PackageVersionMismatch,
    MissingComponent,
    IncompatibleInterfaceVersion,
    MissingOperation,
    DuplicateOperationFact,
    UndeclaredParameter,
    MissingRequiredParameter,
    UnknownOutputPort,
    UnknownInputPort,
    MissingInputPort,
    AmbiguousInputPort,
    RouterRejected,
}

/// A fail-closed catalog-to-router lowering error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WiringLoweringError {
    kind: WiringLoweringErrorKind,
    detail: Box<str>,
    source: Option<WiringError>,
}

impl WiringLoweringError {
    fn new(kind: WiringLoweringErrorKind, detail: impl Into<Box<str>>) -> Self {
        Self {
            kind,
            detail: detail.into(),
            source: None,
        }
    }

    /// The stable classification of this refusal.
    pub fn kind(&self) -> WiringLoweringErrorKind {
        self.kind
    }
}

impl std::fmt::Display for WiringLoweringError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for WiringLoweringError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

impl From<WiringError> for WiringLoweringError {
    fn from(source: WiringError) -> Self {
        Self {
            kind: WiringLoweringErrorKind::RouterRejected,
            detail: format!("router refused lowered wiring: {source}").into_boxed_str(),
            source: Some(source),
        }
    }
}

/// Lower one gated active wiring into the exact graph the router executes.
///
/// The operation-fact scope and exact package version must match the active row.
/// Every document reference is checked before its authoring-only metadata is
/// lowered. In particular, an absent target port is inferred only for a target
/// operation declaring exactly one input, then carried by the router edge to
/// the component invocation while source output ports continue to select
/// successors.
pub fn lower_active_wiring(
    active: GatedActiveWiring<'_>,
    facts: ScopedWiringOperationFacts<'_>,
) -> Result<Wiring, WiringLoweringError> {
    validate_scope(active, facts)?;

    let mut nodes = Vec::with_capacity(active.document.nodes.len());
    let mut resolved = BTreeMap::new();
    for (node_id, node) in &active.document.nodes {
        let fact = resolve_operation(node_id, node, facts.operations)?;
        validate_parameters(node_id, node, fact)?;
        resolved.insert(node_id.as_str(), fact);
        nodes.push(RouterNode {
            id: node_id.clone(),
            component: fact.component_digest.clone(),
            config: Value::Object(Map::from_iter(node.params.clone())),
            // Connection generations are host-injected authority. Neither the
            // wiring nor a component-library operation fact owns a binding.
            connection: None,
            terminal: node.terminal.as_ref().map(lower_terminal),
        });
    }

    let mut edges = Vec::with_capacity(active.document.edges.len());
    for edge in &active.document.edges {
        let source = resolved
            .get(edge.from.as_str())
            .expect("a validated wiring edge source resolves");
        if edge.from_port != ERROR_PORT && !source.output_ports.contains(&edge.from_port) {
            return Err(WiringLoweringError::new(
                WiringLoweringErrorKind::UnknownOutputPort,
                format!(
                    "wiring node {:?} operation {:?} does not declare output port {:?}",
                    edge.from, source.operation, edge.from_port
                ),
            ));
        }

        let target = resolved
            .get(edge.to.as_str())
            .expect("a validated wiring edge target resolves");
        let to_port = resolve_target_port(&edge.to, edge.to_port.as_deref(), target)?;

        edges.push(RouterEdge {
            from: edge.from.clone(),
            from_port: edge.from_port.clone(),
            to: edge.to.clone(),
            to_port,
            // WiringDocument array order is the fan-out order. Wiring::compile
            // uses a stable sort for absent ordinals, preserving these bytes'
            // order without manufacturing a second ordinal contract.
            ordinal: None,
        });
    }

    Wiring::compile(active.document.entry.clone(), nodes, edges).map_err(Into::into)
}

fn validate_scope(
    active: GatedActiveWiring<'_>,
    facts: ScopedWiringOperationFacts<'_>,
) -> Result<(), WiringLoweringError> {
    if active.scope != facts.scope {
        return Err(WiringLoweringError::new(
            WiringLoweringErrorKind::ScopeMismatch,
            format!(
                "active wiring scope ({:?}, {:?}, {:?}) differs from operation-fact scope ({:?}, {:?}, {:?})",
                active.scope.tenant_id,
                active.scope.package_id,
                active.scope.environment,
                facts.scope.tenant_id,
                facts.scope.package_id,
                facts.scope.environment
            ),
        ));
    }
    if active.package_version != facts.package_version {
        return Err(WiringLoweringError::new(
            WiringLoweringErrorKind::PackageVersionMismatch,
            format!(
                "active wiring belongs to package version {:?}, but operation facts are from {:?}",
                active.package_version, facts.package_version
            ),
        ));
    }
    Ok(())
}

fn resolve_operation<'a>(
    node_id: &str,
    node: &CatalogWiringNode,
    facts: &'a [WiringOperationFact],
) -> Result<&'a WiringOperationFact, WiringLoweringError> {
    let component_matches: Vec<_> = facts
        .iter()
        .filter(|fact| fact.component == node.component)
        .collect();
    if component_matches.is_empty() {
        return Err(WiringLoweringError::new(
            WiringLoweringErrorKind::MissingComponent,
            format!(
                "wiring node {node_id:?} names missing component {:?}",
                node.component
            ),
        ));
    }

    let interface_matches: Vec<_> = component_matches
        .into_iter()
        .filter(|fact| fact.interface_version == node.interface_version)
        .collect();
    if interface_matches.is_empty() {
        return Err(WiringLoweringError::new(
            WiringLoweringErrorKind::IncompatibleInterfaceVersion,
            format!(
                "wiring node {node_id:?} component {:?} has no interface version {:?}",
                node.component, node.interface_version
            ),
        ));
    }

    let mut operation_matches = interface_matches
        .into_iter()
        .filter(|fact| fact.operation == node.operation);
    let Some(operation) = operation_matches.next() else {
        return Err(WiringLoweringError::new(
            WiringLoweringErrorKind::MissingOperation,
            format!(
                "wiring node {node_id:?} component {:?} interface {:?} has no operation {:?}",
                node.component, node.interface_version, node.operation
            ),
        ));
    };
    if operation_matches.next().is_some() {
        return Err(WiringLoweringError::new(
            WiringLoweringErrorKind::DuplicateOperationFact,
            format!(
                "wiring node {node_id:?} resolves more than one fact for {:?} {:?} {:?}",
                node.component, node.interface_version, node.operation
            ),
        ));
    }
    Ok(operation)
}

fn validate_parameters(
    node_id: &str,
    node: &CatalogWiringNode,
    fact: &WiringOperationFact,
) -> Result<(), WiringLoweringError> {
    if let Some(parameter) = node
        .params
        .keys()
        .find(|parameter| !fact.parameters.contains_key(*parameter))
    {
        return Err(WiringLoweringError::new(
            WiringLoweringErrorKind::UndeclaredParameter,
            format!("wiring node {node_id:?} supplies undeclared parameter {parameter:?}"),
        ));
    }
    if let Some(parameter) = fact.parameters.iter().find_map(|(name, parameter)| {
        (parameter.required && !node.params.contains_key(name)).then_some(name)
    }) {
        return Err(WiringLoweringError::new(
            WiringLoweringErrorKind::MissingRequiredParameter,
            format!("wiring node {node_id:?} omits required parameter {parameter:?}"),
        ));
    }
    Ok(())
}

fn resolve_target_port(
    node_id: &str,
    authored_port: Option<&str>,
    fact: &WiringOperationFact,
) -> Result<String, WiringLoweringError> {
    match authored_port {
        Some(port) if fact.input_ports.contains(port) => Ok(port.to_owned()),
        Some(port) => Err(WiringLoweringError::new(
            WiringLoweringErrorKind::UnknownInputPort,
            format!(
                "wiring target {node_id:?} operation {:?} does not declare input port {port:?}",
                fact.operation
            ),
        )),
        None if fact.input_ports.len() == 1 => Ok(fact
            .input_ports
            .first()
            .expect("a singleton input-port set has one member")
            .clone()),
        None if fact.input_ports.is_empty() => Err(WiringLoweringError::new(
            WiringLoweringErrorKind::MissingInputPort,
            format!(
                "wiring target {node_id:?} operation {:?} declares no input port",
                fact.operation
            ),
        )),
        None => Err(WiringLoweringError::new(
            WiringLoweringErrorKind::AmbiguousInputPort,
            format!(
                "wiring target {node_id:?} operation {:?} declares {} input ports; to-port is required",
                fact.operation,
                fact.input_ports.len()
            ),
        )),
    }
}

fn lower_terminal(terminal: &CatalogTerminal) -> Terminal {
    match terminal {
        CatalogTerminal::Respond => Terminal::Respond,
        CatalogTerminal::Emit { entity, operation } => Terminal::emit(entity, *operation),
    }
}
