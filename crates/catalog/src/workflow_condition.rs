//! The condition of an environment workflow graph against a serving release
//! (docs/architecture/execution.md#environment-workflows).
//!
//! A workflow graph is a [`WiringDocument`]. It is subject to the conditions of the release
//! an environment serves, not against one package: each node resolves to the
//! component of that release whose descriptor carries the node's component,
//! interface version and operation, and
//! [`validate_resolved_wiring_compatibility`] judges the graph over those
//! per-node facts. A node resolves within the packages of its application
//! first, then within any package of the release. An application is a name
//! the environment declares with its packages
//! ([`crate::EnvironmentApplications`]), never a package. A node whose operation has a registered
//! operation needs that permission in the run's role, validated per node like
//! every other call.
//!
//! Authoring, enable and the host's `workflow.start` admission all condition
//! through this one owner.

use crate::canonical_name::CanonicalName;

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    AdmittedComponent, ComponentPackageCoordinate, ServingManifest, WiringDocument,
    validate_resolved_wiring_compatibility,
};

/// A workflow graph that does not meet the conditions of a serving release.
#[derive(Debug)]
pub struct WorkflowConditionError {
    detail: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl WorkflowConditionError {
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
            source: None,
        }
    }
}

impl std::fmt::Display for WorkflowConditionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for WorkflowConditionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}

/// Resolve each node of `document` to the component of `manifest` that serves
/// it, as the facts [`validate_resolved_wiring_compatibility`] reads.
///
/// A node resolves to a component whose descriptor carries the node's
/// component, interface version and operation. It resolves within
/// `application_packages`, the packages the environment declares for the
/// workflow's application, first, then within any package of the release;
/// two different digests there refuse as ambiguous. A declared package the
/// release does not carry refuses.
pub fn resolve_workflow_components(
    project_env_key: &wamn_registry::ProjectEnvKey,
    application_packages: &BTreeSet<crate::PackageName>,
    manifest: &ServingManifest,
    document: &WiringDocument,
) -> Result<BTreeMap<CanonicalName, AdmittedComponent>, WorkflowConditionError> {
    let versions: BTreeMap<&crate::PackageName, &crate::package_version::PackageVersion> = manifest
        .release
        .packages
        .iter()
        .map(|package| (package.package_id(), package.package_version()))
        .collect();
    if let Some(package) = application_packages
        .iter()
        .find(|package| !versions.contains_key(*package))
    {
        return Err(WorkflowConditionError::new(format!(
            "the serving release does not carry package {package:?} of the application"
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
            .filter(|component| application_packages.contains(&component.package_id))
            .collect();
        let chosen = match (in_application.as_slice(), serving.as_slice()) {
            ([component], _) => **component,
            ([], []) => {
                return Err(WorkflowConditionError::new(format!(
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
        let target = ComponentPackageCoordinate {
            project_env_key: project_env_key.to_owned(),
            package_id: chosen.package_id.clone(),
            package_version: if chosen.package_id.as_str() == crate::PALETTE_PACKAGE_ID {
                crate::PALETTE_PACKAGE_VERSION
                    .parse()
                    .expect("fixed palette package version")
            } else {
                versions[&chosen.package_id].clone()
            },
        };
        resolved.insert(
            node_id.clone(),
            chosen.descriptor.clone().into_admitted(target).component,
        );
    }
    Ok(resolved)
}

fn ambiguous(node_id: &CanonicalName) -> WorkflowConditionError {
    WorkflowConditionError::new(format!(
        "workflow node {node_id:?} resolves to more than one component of the release"
    ))
}

/// Condition `document` against the per-node facts of `manifest` and derive its
/// hash. The hash is produced only on the far side of the condition.
pub fn condition_workflow_document(
    project_env_key: &wamn_registry::ProjectEnvKey,
    application_packages: &BTreeSet<crate::PackageName>,
    manifest: &ServingManifest,
    document: &WiringDocument,
) -> Result<wamn_digest::Digest, WorkflowConditionError> {
    let components =
        resolve_workflow_components(project_env_key, application_packages, manifest, document)?;
    validate_resolved_wiring_compatibility(document, &components).map_err(|error| {
        WorkflowConditionError {
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
    use super::*;
    use crate::{
        AdmittedComponentOperation, PackageCoordinate, ServingComponent, ServingRelease,
        WiringNode, WiringTerminal,
    };

    const APPLICATION: &str = "orders";

    /// The packages of the workflow's application.
    fn packages() -> BTreeSet<crate::PackageName> {
        BTreeSet::from([APPLICATION.parse().unwrap()])
    }
    const ORDERS_GET: &str = "orders:order/get@1.0.0";
    const PALETTE_GET: &str = "palette:data/get@2.0.0";

    fn operation(op: &str, registered: bool) -> AdmittedComponentOperation {
        AdmittedComponentOperation {
            committed_result_schema: None,
            registered_operation: registered.then(|| op.parse().unwrap()),
            host_transaction: false,
            input_ports: Vec::new(),
            output_ports: Vec::new(),
            parameters: Vec::new(),
        }
    }

    fn component(
        package: &crate::PackageName,
        name: &str,
        op: &str,
        registered: bool,
    ) -> ServingComponent {
        component_with_digest(package, name, op, registered, '1')
    }

    fn component_with_digest(
        package: &crate::PackageName,
        name: &str,
        op: &str,
        registered: bool,
        digest: char,
    ) -> ServingComponent {
        let fact = AdmittedComponent {
            target: ComponentPackageCoordinate {
                project_env_key: "t1--dev--project".parse().unwrap(),
                package_id: package.to_owned(),
                package_version: "1.0.0".parse().unwrap(),
            },
            component: name.parse().unwrap(),
            interface_version: "0.1".parse().unwrap(),
            operations: BTreeMap::from([(op.parse().unwrap(), operation(op, registered))]),
            component_digest: format!("sha256:{}", digest.to_string().repeat(64))
                .parse()
                .unwrap(),
            imports: Vec::new(),
            imports_fingerprint: (format!("sha256:{}", "6".repeat(64)))
                .parse::<wamn_digest::Digest>()
                .unwrap(),
            effects: Vec::new(),
        };
        ServingComponent::project(&fact, Vec::new())
    }

    fn manifest(components: Vec<ServingComponent>) -> ServingManifest {
        ServingManifest::new(
            ServingRelease {
                packages: BTreeSet::from([
                    PackageCoordinate::new(APPLICATION.parse().unwrap(), "1.0.0".parse().unwrap())
                        .unwrap(),
                    PackageCoordinate::new("palette".parse().unwrap(), "2.0.0".parse().unwrap())
                        .unwrap(),
                ]),
            },
            components.into_iter().collect(),
            BTreeSet::new(),
            BTreeMap::new(),
        )
        .expect("the fixture manifest is valid")
    }

    fn document(name: &str, op: &str) -> WiringDocument {
        WiringDocument::new(
            "nightly".parse().unwrap(),
            1,
            "node".parse().unwrap(),
            BTreeMap::from([(
                "node".parse().unwrap(),
                WiringNode {
                    component: name.parse().unwrap(),
                    interface_version: "0.1".parse().unwrap(),
                    operation: op.parse().unwrap(),
                    operation_dependency: None,
                    params: BTreeMap::new(),
                    terminal: Some(WiringTerminal::Respond),
                },
            )]),
            Vec::new(),
        )
        .expect("the fixture workflow is structurally valid")
    }

    #[test]
    fn an_application_operation_conditions_with_its_target() {
        let manifest = manifest(vec![component(
            &APPLICATION.parse().unwrap(),
            "data",
            ORDERS_GET,
            true,
        )]);
        let document = document("data", ORDERS_GET);
        let resolved = resolve_workflow_components(
            &"t1--dev--project".parse().unwrap(),
            &packages(),
            &manifest,
            &document,
        )
        .unwrap();
        assert_eq!(
            resolved["node"].target.package_version,
            "1.0.0".parse().unwrap()
        );
        assert_eq!(
            condition_workflow_document(
                &"t1--dev--project".parse().unwrap(),
                &packages(),
                &manifest,
                &document
            )
            .unwrap(),
            document.wiring_hash()
        );
    }

    #[test]
    fn a_platform_node_resolves_in_another_package() {
        let manifest = manifest(vec![component(
            &"palette".parse().unwrap(),
            "jsonata",
            "map",
            false,
        )]);
        let resolved = resolve_workflow_components(
            &"t1--dev--project".parse().unwrap(),
            &packages(),
            &manifest,
            &document("jsonata", "map"),
        )
        .unwrap();
        assert_eq!(
            resolved["node"].target.package_id,
            "palette".parse().unwrap()
        );
    }

    #[test]
    fn an_operation_of_another_package_resolves_in_that_package() {
        let manifest = manifest(vec![component(
            &"palette".parse().unwrap(),
            "data",
            PALETTE_GET,
            true,
        )]);
        let resolved = resolve_workflow_components(
            &"t1--dev--project".parse().unwrap(),
            &packages(),
            &manifest,
            &document("data", PALETTE_GET),
        )
        .unwrap();
        assert_eq!(
            resolved["node"].target.package_id,
            "palette".parse().unwrap()
        );
    }

    #[test]
    fn a_node_the_release_does_not_carry_refuses() {
        let manifest = manifest(vec![component(
            &APPLICATION.parse().unwrap(),
            "data",
            ORDERS_GET,
            true,
        )]);
        for document in [document("data", "put"), document("other", ORDERS_GET)] {
            condition_workflow_document(
                &"t1--dev--project".parse().unwrap(),
                &packages(),
                &manifest,
                &document,
            )
            .expect_err("a node absent from the release refuses");
        }
        condition_workflow_document(
            &"t1--dev--project".parse().unwrap(),
            &BTreeSet::from(["absent".parse().unwrap()]),
            &manifest,
            &document("data", ORDERS_GET),
        )
        .expect_err("a declared package absent from the release refuses");
    }

    /// The application's packages win over another package that serves the
    /// same node with another digest.
    #[test]
    fn a_node_resolves_within_the_application_packages_first() {
        let other =
            component_with_digest(&"palette".parse().unwrap(), "jsonata", "map", false, '2');
        let manifest = manifest(vec![
            component(&APPLICATION.parse().unwrap(), "jsonata", "map", false),
            other,
        ]);
        let document = document("jsonata", "map");
        let resolved = resolve_workflow_components(
            &"t1--dev--project".parse().unwrap(),
            &packages(),
            &manifest,
            &document,
        )
        .unwrap();
        assert_eq!(
            resolved["node"].target.package_id,
            APPLICATION.parse().unwrap()
        );
        resolve_workflow_components(
            &"t1--dev--project".parse().unwrap(),
            &BTreeSet::new(),
            &manifest,
            &document,
        )
        .expect_err("without the application's packages the node is ambiguous");
    }
}
