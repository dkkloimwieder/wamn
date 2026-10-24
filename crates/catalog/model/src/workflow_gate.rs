//! The gate of an environment workflow graph against a serving release
//! (docs/plan/workflows.md R8, epic 1).
//!
//! A workflow graph is a [`WiringDocument`]. It is gated against the release
//! an environment serves, not against one package: each node resolves to the
//! component of that release whose descriptor carries the node's component,
//! interface version and operation, and
//! [`validate_resolved_wiring_compatibility`] judges the graph over those
//! per-node facts. A node resolves within the application first, then within
//! any package of the release. A node whose operation has a registered
//! operation needs that permission in the run's role, checked per node like
//! every other call.
//!
//! Authoring, enable and the host's `workflow.start` admission all gate
//! through this one owner.

use std::collections::BTreeMap;

use crate::{
    AdmittedComponent, ComponentPackageScope, DefinitionHash, ServingManifest, WiringDocument,
    validate_resolved_wiring_compatibility,
};

/// A workflow graph that does not gate against a serving release.
#[derive(Debug)]
pub struct WorkflowGateError {
    detail: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl WorkflowGateError {
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
            source: None,
        }
    }
}

impl std::fmt::Display for WorkflowGateError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for WorkflowGateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}

/// Resolve each node of `document` to the component of `manifest` that serves
/// it, as the facts [`validate_resolved_wiring_compatibility`] reads.
///
/// A node resolves to a component whose descriptor carries the node's
/// component, interface version and operation. It resolves within the
/// application first, then within any package of the release; two different
/// digests there refuse as ambiguous.
pub fn resolve_workflow_components(
    tenant_id: &str,
    application: &str,
    manifest: &ServingManifest,
    document: &WiringDocument,
) -> Result<BTreeMap<String, AdmittedComponent>, WorkflowGateError> {
    let versions: BTreeMap<&str, &str> = manifest
        .release
        .packages
        .iter()
        .map(|package| (package.package_id(), package.package_version()))
        .collect();
    if !versions.contains_key(application) {
        return Err(WorkflowGateError::new(format!(
            "the serving release carries no application {application:?}"
        )));
    }
    let mut resolved = BTreeMap::new();
    for (node_id, node) in &document.nodes {
        let serving: Vec<_> = manifest
            .components
            .iter()
            .filter(|component| {
                component.descriptor.component == node.component
                    && component.descriptor.interface_version == node.interface_version
                    && component
                        .descriptor
                        .operations
                        .contains_key(&node.operation)
            })
            .collect();
        let in_application: Vec<_> = serving
            .iter()
            .filter(|component| component.package_id == application)
            .collect();
        let chosen = match (in_application.as_slice(), serving.as_slice()) {
            ([component], _) => **component,
            ([], []) => {
                return Err(WorkflowGateError::new(format!(
                    "workflow node {node_id:?} names component {:?} interface {:?} \
                     operation {:?}, which the serving release does not carry",
                    node.component, node.interface_version, node.operation
                )));
            }
            ([], [first, rest @ ..]) => {
                if rest.iter().any(|other| {
                    other.descriptor.component_digest != first.descriptor.component_digest
                }) {
                    return Err(ambiguous(node_id));
                }
                *first
            }
            _ => return Err(ambiguous(node_id)),
        };
        let scope = ComponentPackageScope {
            tenant_id: tenant_id.to_owned(),
            package_id: chosen.package_id.clone(),
            package_version: versions[chosen.package_id.as_str()].to_owned(),
        };
        resolved.insert(
            node_id.clone(),
            chosen.descriptor.clone().into_admitted(scope).component,
        );
    }
    Ok(resolved)
}

fn ambiguous(node_id: &str) -> WorkflowGateError {
    WorkflowGateError::new(format!(
        "workflow node {node_id:?} resolves to more than one component of the release"
    ))
}

/// Gate `document` against the per-node facts of `manifest` and derive its
/// hash. The hash is produced only on the far side of the gate.
pub fn gate_workflow_document(
    tenant_id: &str,
    application: &str,
    manifest: &ServingManifest,
    document: &WiringDocument,
) -> Result<DefinitionHash, WorkflowGateError> {
    let components = resolve_workflow_components(tenant_id, application, manifest, document)?;
    validate_resolved_wiring_compatibility(document, &components).map_err(|error| {
        WorkflowGateError {
            detail: format!(
                "workflow {:?} version {} is not compatible with the serving release",
                document.wiring_id, document.version
            ),
            source: Some(Box::new(error)),
        }
    })?;
    Ok(document.wiring_hash())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::{
        AdmittedComponentOperation, PackageCoordinate, ServingComponent, ServingRelease,
        WiringNode, WiringTerminal,
    };

    const APPLICATION: &str = "orders";
    const ORDERS_GET: &str = "orders:order/get@1.0.0";
    const PALETTE_GET: &str = "palette:data/get@2.0.0";

    fn operation(op: &str, registered: bool) -> AdmittedComponentOperation {
        AdmittedComponentOperation {
            committed_result_schema: None,
            registered_operation: registered.then(|| op.to_owned()),
            host_transaction: false,
            input_ports: Vec::new(),
            output_ports: Vec::new(),
            parameters: Vec::new(),
            statements: BTreeMap::new(),
        }
    }

    fn component(package: &str, name: &str, op: &str, registered: bool) -> ServingComponent {
        let fact = AdmittedComponent {
            scope: ComponentPackageScope {
                tenant_id: "t1".to_owned(),
                package_id: package.to_owned(),
                package_version: "1.0.0".to_owned(),
            },
            component: name.to_owned(),
            interface_version: "0.1".to_owned(),
            operations: BTreeMap::from([(op.to_owned(), operation(op, registered))]),
            component_digest: format!("sha256:{}", "1".repeat(64)),
            imports: Vec::new(),
            imports_fingerprint: format!("sha256:{}", "6".repeat(64)),
            effects: Vec::new(),
        };
        ServingComponent::project(&fact, Vec::new()).expect("the fixture component projects")
    }

    fn manifest(components: Vec<ServingComponent>) -> ServingManifest {
        ServingManifest::new(
            ServingRelease {
                packages: BTreeSet::from([
                    PackageCoordinate::new(APPLICATION, "1.0.0").unwrap(),
                    PackageCoordinate::new("palette", "2.0.0").unwrap(),
                ]),
            },
            components.into_iter().collect(),
            BTreeSet::new(),
            BTreeSet::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        )
        .expect("the fixture manifest is valid")
    }

    fn document(name: &str, op: &str) -> WiringDocument {
        WiringDocument::new(
            "nightly",
            1,
            "node",
            BTreeMap::from([(
                "node".to_owned(),
                WiringNode {
                    component: name.to_owned(),
                    interface_version: "0.1".to_owned(),
                    operation: op.to_owned(),
                    operation_dependency: None,
                    params: BTreeMap::new(),
                    terminal: Some(WiringTerminal::Respond),
                },
            )]),
            Vec::new(),
            Vec::new(),
        )
        .expect("the fixture workflow is structurally valid")
    }

    #[test]
    fn an_application_operation_gates_with_its_scope() {
        let manifest = manifest(vec![component(APPLICATION, "data", ORDERS_GET, true)]);
        let document = document("data", ORDERS_GET);
        let resolved =
            resolve_workflow_components("t1", APPLICATION, &manifest, &document).unwrap();
        assert_eq!(resolved["node"].scope.package_version, "1.0.0");
        assert_eq!(
            gate_workflow_document("t1", APPLICATION, &manifest, &document).unwrap(),
            document.wiring_hash()
        );
    }

    #[test]
    fn a_platform_node_resolves_in_another_package() {
        let manifest = manifest(vec![component("palette", "jsonata", "map", false)]);
        let resolved =
            resolve_workflow_components("t1", APPLICATION, &manifest, &document("jsonata", "map"))
                .unwrap();
        assert_eq!(resolved["node"].scope.package_id, "palette");
    }

    #[test]
    fn an_operation_of_another_package_resolves_in_that_package() {
        let manifest = manifest(vec![component("palette", "data", PALETTE_GET, true)]);
        let resolved = resolve_workflow_components(
            "t1",
            APPLICATION,
            &manifest,
            &document("data", PALETTE_GET),
        )
        .unwrap();
        assert_eq!(resolved["node"].scope.package_id, "palette");
    }

    #[test]
    fn a_node_the_release_does_not_carry_refuses() {
        let manifest = manifest(vec![component(APPLICATION, "data", ORDERS_GET, true)]);
        for document in [document("data", "put"), document("other", ORDERS_GET)] {
            gate_workflow_document("t1", APPLICATION, &manifest, &document)
                .expect_err("a node absent from the release refuses");
        }
        gate_workflow_document("t1", "absent", &manifest, &document("data", ORDERS_GET))
            .expect_err("an application absent from the release refuses");
    }
}
