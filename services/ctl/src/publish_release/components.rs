//! Exact component facts and their operation dependencies.

use super::{
    AdmittedComponent, ArtifactHash, BTreeMap, BTreeSet, ComponentPackageScope,
    DependencyDigestRule, MintManifestError, MintManifestErrorKind, ReleaseWiringTarget,
    ServingAttachment, ServingComponent, ServingComponentOperation, VecDeque, WiringDocument,
};

pub(super) fn resolve_wiring_components(
    document: &WiringDocument,
    owner: &ComponentPackageScope,
    component_facts: &BTreeMap<(String, String), Vec<AdmittedComponent>>,
    package_manifest: Option<&wamn_schema_generator::PackageManifest>,
    rule: DependencyDigestRule,
) -> Result<BTreeMap<String, AdmittedComponent>, MintManifestError> {
    let mut resolved = BTreeMap::new();
    for (node_id, node) in &document.nodes {
        let (package_id, package_version, digest, registered_operation) = match &node
            .operation_dependency
        {
            None => (
                owner.package_id.as_str(),
                owner.package_version.as_str(),
                None,
                None,
            ),
            Some(dependency) => {
                let manifest = package_manifest.ok_or_else(|| {
                        MintManifestError::new(
                            MintManifestErrorKind::OperationDependency,
                            format!(
                                "wiring node {node_id:?} invokes dependency alias {:?}, but package {}@{} supplied no package manifest",
                                dependency.alias, owner.package_id, owner.package_version
                            ),
                        )
                    })?;
                if manifest.package.id != owner.package_id
                    || manifest.package.version != owner.package_version
                {
                    return Err(MintManifestError::new(
                        MintManifestErrorKind::OperationDependency,
                        format!(
                            "wiring node {node_id:?} dependency manifest coordinate differs from {}@{}",
                            owner.package_id, owner.package_version
                        ),
                    ));
                }
                let requirement = manifest
                    .base_dependencies
                    .get(&dependency.alias)
                    .ok_or_else(|| {
                        MintManifestError::new(
                            MintManifestErrorKind::OperationDependency,
                            format!(
                                "wiring node {node_id:?} names undeclared dependency alias {:?}",
                                dependency.alias
                            ),
                        )
                    })?;
                if !requirement.operations.contains(&dependency.operation) {
                    return Err(MintManifestError::new(
                        MintManifestErrorKind::OperationDependency,
                        format!(
                            "wiring node {node_id:?} operation {:?} is absent from dependency alias {:?}",
                            dependency.operation, dependency.alias
                        ),
                    ));
                }
                let dependency_package = wamn_schema_generator::PackageIdentity {
                    id: requirement.package.clone(),
                    version: requirement.version.clone(),
                    predecessor_version: None,
                };
                let registered_operation = wamn_schema_generator::canonical_operation_identity(
                    &dependency_package,
                    &dependency.operation,
                )
                .map_err(|error| {
                    MintManifestError::with_source(
                        MintManifestErrorKind::OperationDependency,
                        format!("wiring node {node_id:?} dependency operation is not canonical"),
                        error,
                    )
                })?;
                (
                    requirement.package.as_str(),
                    requirement.version.as_str(),
                    rule.matches_declared_digest()
                        .then_some(requirement.digest.as_str()),
                    Some(registered_operation),
                )
            }
        };
        let facts = component_facts
            .get(&(package_id.to_owned(), package_version.to_owned()))
            .ok_or_else(|| {
                MintManifestError::new(
                    MintManifestErrorKind::OperationDependency,
                    format!(
                        "wiring node {node_id:?} target package {package_id}@{package_version} is absent from the effective release"
                    ),
                )
            })?;
        let mut matches = facts.iter().filter(|fact| {
            fact.component == node.component
                && fact.interface_version == node.interface_version
                && digest.is_none_or(|digest| fact.component_digest == digest)
                && fact.operation(&node.operation).is_some_and(|declared| {
                    registered_operation.as_deref().is_none_or(|operation| {
                        declared.registered_operation.as_deref() == Some(operation)
                    })
                })
        });
        let Some(component) = matches.next() else {
            let kind = if node.operation_dependency.is_some() {
                MintManifestErrorKind::OperationDependency
            } else {
                MintManifestErrorKind::Component
            };
            return Err(MintManifestError::new(
                kind,
                format!(
                    "wiring node {node_id:?} has no exact component tuple in {package_id}@{package_version}"
                ),
            ));
        };
        if matches.next().is_some() {
            return Err(MintManifestError::new(
                MintManifestErrorKind::Component,
                format!("wiring node {node_id:?} resolves more than one exact component fact"),
            ));
        }
        resolved.insert(node_id.clone(), component.clone());
    }
    Ok(resolved)
}

pub(super) fn resolved_wiring_entry_operation(
    document: &WiringDocument,
    resolved: &BTreeMap<String, AdmittedComponent>,
) -> Result<String, MintManifestError> {
    let entry = &document.nodes[&document.entry];
    let component = resolved.get(&document.entry).ok_or_else(|| {
        MintManifestError::new(
            MintManifestErrorKind::Component,
            format!(
                "wiring entry {:?} has no resolved component",
                document.entry
            ),
        )
    })?;
    if component.operation(&entry.operation).is_none() {
        return Err(MintManifestError::new(
            MintManifestErrorKind::Component,
            format!(
                "wiring entry {:?} has no resolved export {:?}",
                document.entry, entry.operation
            ),
        ));
    }
    Ok(entry.operation.clone())
}

type ComponentOperationKey = (String, String, String);

pub(super) fn resolve_component_dependency_closure(
    roots: &BTreeMap<String, AdmittedComponent>,
    component_facts: &BTreeMap<(String, String), Vec<AdmittedComponent>>,
    rule: DependencyDigestRule,
) -> Result<Vec<AdmittedComponent>, MintManifestError> {
    let mut pending = roots.values().cloned().collect::<Vec<_>>();
    let mut closure = BTreeMap::<(String, String, String), AdmittedComponent>::new();
    while let Some(component) = pending.pop() {
        let key = (
            component.scope.package_id.clone(),
            component.scope.package_version.clone(),
            component.component_digest.clone(),
        );
        if let Some(existing) = closure.get(&key) {
            if existing != &component {
                return Err(MintManifestError::new(
                    MintManifestErrorKind::OperationDependency,
                    format!("component dependency tuple {key:?} resolves more than one fact"),
                ));
            }
            continue;
        }
        for operation in component.operations.values() {
            for dependency in &operation.dependencies {
                pending
                    .push(resolve_component_dependency(dependency, component_facts, rule)?.clone());
            }
        }
        closure.insert(key, component);
    }
    validate_component_dependency_cycles(closure.values(), component_facts, rule)?;
    Ok(closure.into_values().collect())
}

fn resolve_component_dependency<'a>(
    dependency: &wamn_catalog::ComponentOperationDependency,
    component_facts: &'a BTreeMap<(String, String), Vec<AdmittedComponent>>,
    rule: DependencyDigestRule,
) -> Result<&'a AdmittedComponent, MintManifestError> {
    let Some(facts) =
        component_facts.get(&(dependency.package.clone(), dependency.version.clone()))
    else {
        return Err(MintManifestError::new(
            MintManifestErrorKind::OperationDependency,
            format!(
                "component dependency {}@{} is absent from the effective release",
                dependency.package, dependency.version
            ),
        ));
    };
    let mut matches = facts.iter().filter(|component| {
        (!rule.matches_declared_digest() || component.component_digest == dependency.digest)
            && component
                .operation(&dependency.operation)
                .is_some_and(|operation| {
                    operation.registered_operation.as_deref() == Some(dependency.operation.as_str())
                })
    });
    let Some(component) = matches.next() else {
        return Err(MintManifestError::new(
            MintManifestErrorKind::OperationDependency,
            format!(
                "component dependency {}@{} digest {} operation {:?} has no exact admitted fact",
                dependency.package, dependency.version, dependency.digest, dependency.operation
            ),
        ));
    };
    if matches.next().is_some() {
        return Err(MintManifestError::new(
            MintManifestErrorKind::OperationDependency,
            format!(
                "component dependency {}@{} digest {} operation {:?} resolves more than one admitted fact",
                dependency.package, dependency.version, dependency.digest, dependency.operation
            ),
        ));
    }
    Ok(component)
}

/// Name the declared operation dependencies whose admitted facts show no effects.
///
/// `ComponentAdmissionRequest::effect_free_operation_dependencies` is
/// fail-closed, so a caller that names nothing leaves every dependency
/// effectful. This is the evidence a caller holding the admitted facts can supply.
///
/// One lookup answers the whole closure. A dependency's own admitted row
/// carries the effect projection of everything that dependency reaches, because
/// admission computed that row under this same rule, so an empty effects array
/// is sufficient evidence. A dependency stays out of the set if its facts do not
/// resolve exactly. The exact-resolution refusal belongs to
/// `resolve_component_dependency_closure`, which walks the same facts when the
/// release is minted.
pub fn effect_free_operation_dependencies(
    declaration: &wamn_catalog::ComponentDeclaration,
    component_facts: &BTreeMap<(String, String), Vec<AdmittedComponent>>,
    rule: DependencyDigestRule,
) -> BTreeSet<String> {
    let mut dependencies = BTreeSet::new();
    for operation in declaration.operations.values() {
        for dependency in &operation.dependencies {
            if resolve_component_dependency(dependency, component_facts, rule)
                .is_ok_and(|component| component.effects.is_empty())
            {
                dependencies.insert(dependency.operation.clone());
            }
        }
    }
    dependencies
}

/// Every edge is keyed by the fact it RESOLVES TO, never by the digest the
/// declaration named. Under the durable rule the two are the same value,
/// because resolution demanded equality. Under the development rule they differ
/// exactly when a base package was edited, which is the case wamn-10yt.48 is
/// about, and keying on the declaration there names a node that is not in the
/// graph.
fn validate_component_dependency_cycles<'a>(
    components: impl Iterator<Item = &'a AdmittedComponent>,
    component_facts: &BTreeMap<(String, String), Vec<AdmittedComponent>>,
    rule: DependencyDigestRule,
) -> Result<(), MintManifestError> {
    let mut graph = BTreeMap::<ComponentOperationKey, Vec<ComponentOperationKey>>::new();
    for component in components {
        for (operation_name, operation) in &component.operations {
            let key = (
                component.scope.package_id.clone(),
                component.component_digest.clone(),
                operation_name.clone(),
            );
            let dependencies = operation
                .dependencies
                .iter()
                .map(|dependency| {
                    let resolved = resolve_component_dependency(dependency, component_facts, rule)?;
                    Ok((
                        resolved.scope.package_id.clone(),
                        resolved.component_digest.clone(),
                        dependency.operation.clone(),
                    ))
                })
                .collect::<Result<Vec<_>, MintManifestError>>()?;
            if graph.insert(key.clone(), dependencies).is_some() {
                return Err(MintManifestError::new(
                    MintManifestErrorKind::OperationDependency,
                    format!("component operation tuple {key:?} occurs more than once"),
                ));
            }
        }
    }
    let mut incoming = BTreeMap::new();
    for dependencies in graph.values() {
        for dependency in dependencies {
            *incoming.entry(dependency.clone()).or_insert(0_usize) += 1;
        }
    }
    for operation in graph.keys() {
        incoming.entry(operation.clone()).or_insert(0);
    }
    let mut pending = incoming
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(operation, _)| operation.clone())
        .collect::<VecDeque<_>>();
    let mut visited = 0_usize;
    while let Some(operation) = pending.pop_front() {
        visited += 1;
        for dependency in &graph[&operation] {
            let count = incoming
                .get_mut(dependency)
                .expect("exact dependency resolution populated every target");
            *count -= 1;
            if *count == 0 {
                pending.push_back(dependency.clone());
            }
        }
    }
    if visited != graph.len() {
        return Err(MintManifestError::new(
            MintManifestErrorKind::OperationDependency,
            "component operation dependency closure contains a cycle",
        ));
    }
    Ok(())
}

pub(super) fn project_serving_component(
    fact: &AdmittedComponent,
) -> Result<ServingComponent, MintManifestError> {
    let digest = ArtifactHash::parse(fact.component_digest.clone()).map_err(|error| {
        MintManifestError::with_source(
            MintManifestErrorKind::Component,
            format!(
                "component {:?} stores a non-canonical digest",
                fact.component
            ),
            error,
        )
    })?;
    Ok(ServingComponent {
        package_id: fact.scope.package_id.clone(),
        component: fact.component.clone(),
        interface_version: fact.interface_version.clone(),
        digest,
        operations: fact
            .operations
            .iter()
            .map(|(name, operation)| {
                (
                    name.clone(),
                    ServingComponentOperation {
                        committed_result_schema: operation.committed_result_schema.as_ref().map(
                            |schema| {
                                String::from_utf8(wamn_execution_contract::canonical_json_bytes(
                                    &schema.schema,
                                ))
                                .expect("canonical JSON uses UTF-8")
                            },
                        ),
                        registered_operation: operation.registered_operation.clone(),
                        fresh_only: operation.fresh_only,
                        dependencies: operation.dependencies.clone(),
                        statements: operation.statements.clone(),
                    },
                )
            })
            .collect(),
    })
}

/// Refuse an anonymous attachment whose selected wiring can reach a registered
/// application operation.
///
/// The attachment itself cannot carry this fact for nested calls: reachability
/// exists only in the exact stored wiring plus its admitted component facts, so
/// release mint is the first boundary that can make the invalid composition
/// unrepresentable without adding another manifest field (`wamn-10yt.3.2`).
pub(super) fn validate_anonymous_wiring_closure(
    attachments: &BTreeMap<String, ServingAttachment>,
    target: &ReleaseWiringTarget,
    document: &WiringDocument,
    component_facts: &BTreeMap<String, AdmittedComponent>,
) -> Result<(), MintManifestError> {
    let anonymous_attachments = attachments.iter().filter(|(_, attachment)| {
        attachment.package_id == target.package_id
            && attachment.wiring_id == target.wiring_id
            && attachment.wiring_version == target.wiring_version
            && wamn_catalog::parse_attachment_auth_policy(&attachment.auth_policy)
                == Some(wamn_catalog::AttachmentAuthPolicy::None)
    });
    let reachable = reachable_nodes(document);
    for (attachment_id, _) in anonymous_attachments {
        for node_id in document
            .nodes
            .keys()
            .filter(|node_id| reachable.contains(*node_id))
        {
            let fact = component_facts
                .get(node_id)
                .expect("wiring compatibility resolved every reachable node");
            let operation = fact
                .operation(&document.nodes[node_id].operation)
                .expect("wiring compatibility resolved every reachable operation");
            if let Some(operation) = operation.registered_operation.as_deref() {
                return Err(MintManifestError::new(
                    MintManifestErrorKind::UnauthenticatedRegisteredOperation,
                    format!(
                        "attachment {attachment_id:?} reaches registered operation \
                         {operation:?} at node {node_id:?}; set auth-policy modes = \
                         [{mode:?}]",
                        mode = wamn_catalog::PAT_AUTHENTICATION_MODE,
                    ),
                ));
            }
            if let Some(dependency) = operation.dependencies.first() {
                return Err(MintManifestError::new(
                    MintManifestErrorKind::UnauthenticatedRegisteredOperation,
                    format!(
                        "attachment {attachment_id:?} reaches registered operation \
                         {:?} through component dependency at node {node_id:?}; set \
                         auth-policy modes = [{mode:?}]",
                        dependency.operation,
                        mode = wamn_catalog::PAT_AUTHENTICATION_MODE,
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn reachable_nodes(document: &WiringDocument) -> BTreeSet<String> {
    let mut reachable = BTreeSet::new();
    let mut pending = VecDeque::from([document.entry.as_str()]);
    while let Some(node_id) = pending.pop_front() {
        if !reachable.insert(node_id.to_owned()) {
            continue;
        }
        pending.extend(
            document
                .edges
                .iter()
                .filter(|edge| edge.from == node_id)
                .map(|edge| edge.to.as_str()),
        );
    }
    reachable
}
