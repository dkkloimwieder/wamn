//! Exact component facts and the bases their packages pin.

use wamn_catalog::AdmittedComponentOperation;

use super::{
    AdmittedComponent, BTreeMap, BTreeSet, ComponentPackageScope, DependencyDigestRule,
    PublishManifestError, PublishManifestErrorType, ReleaseWiringTarget, ServingAttachment,
    ServingComponent, VecDeque, WiringDocument,
};

pub(super) fn resolve_wiring_components(
    document: &WiringDocument,
    owner: &ComponentPackageScope,
    component_facts: &BTreeMap<(String, String), Vec<AdmittedComponent>>,
    package_manifest: Option<&wamn_schema_generator::PackageManifest>,
    rule: DependencyDigestRule,
) -> Result<BTreeMap<String, AdmittedComponent>, PublishManifestError> {
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
                        PublishManifestError::new(
                            PublishManifestErrorType::OperationDependency,
                            format!(
                                "wiring node {node_id:?} invokes dependency alias {:?}, but package {}@{} supplied no package manifest",
                                dependency.alias, owner.package_id, owner.package_version
                            ),
                        )
                    })?;
                if manifest.package.id != owner.package_id
                    || manifest.package.version != owner.package_version
                {
                    return Err(PublishManifestError::new(
                        PublishManifestErrorType::OperationDependency,
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
                        PublishManifestError::new(
                            PublishManifestErrorType::OperationDependency,
                            format!(
                                "wiring node {node_id:?} names undeclared dependency alias {:?}",
                                dependency.alias
                            ),
                        )
                    })?;
                if !requirement.operations.contains(&dependency.operation) {
                    return Err(PublishManifestError::new(
                        PublishManifestErrorType::OperationDependency,
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
                    PublishManifestError::with_source(
                        PublishManifestErrorType::OperationDependency,
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
                PublishManifestError::new(
                    PublishManifestErrorType::OperationDependency,
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
                PublishManifestErrorType::OperationDependency
            } else {
                PublishManifestErrorType::Component
            };
            return Err(PublishManifestError::new(
                kind,
                format!(
                    "wiring node {node_id:?} has no exact component tuple in {package_id}@{package_version}"
                ),
            ));
        };
        if matches.next().is_some() {
            return Err(PublishManifestError::new(
                PublishManifestErrorType::Component,
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
) -> Result<String, PublishManifestError> {
    let entry = &document.nodes[&document.entry];
    let component = resolved.get(&document.entry).ok_or_else(|| {
        PublishManifestError::new(
            PublishManifestErrorType::Component,
            format!(
                "wiring entry {:?} has no resolved component",
                document.entry
            ),
        )
    })?;
    if component.operation(&entry.operation).is_none() {
        return Err(PublishManifestError::new(
            PublishManifestErrorType::Component,
            format!(
                "wiring entry {:?} has no resolved export {:?}",
                document.entry, entry.operation
            ),
        ));
    }
    Ok(entry.operation.clone())
}

/// The components of `roots` and of every base their packages pin in
/// `base_dependencies`, transitively.
///
/// A base composes into its overlay's bytes at build, so the release lists
/// only the overlay; this proves that each pinned base is an admitted fact of
/// the effective release, under `rule`, and that no package pins itself
/// through its bases.
pub(super) fn resolve_component_dependency_closure(
    roots: &BTreeMap<String, AdmittedComponent>,
    component_facts: &BTreeMap<(String, String), Vec<AdmittedComponent>>,
    package_manifests: &BTreeMap<String, wamn_schema_generator::PackageManifest>,
    rule: DependencyDigestRule,
) -> Result<Vec<AdmittedComponent>, PublishManifestError> {
    let mut closure = BTreeMap::<(String, String, String), AdmittedComponent>::new();
    let mut visited = BTreeSet::new();
    for component in roots.values() {
        insert_closure_component(&mut closure, component)?;
        visit_base_dependencies(
            (
                &component.scope.package_id,
                &component.scope.package_version,
            ),
            component_facts,
            package_manifests,
            rule,
            &mut Vec::new(),
            &mut visited,
            &mut closure,
        )?;
    }
    Ok(closure.into_values().collect())
}

fn insert_closure_component(
    closure: &mut BTreeMap<(String, String, String), AdmittedComponent>,
    component: &AdmittedComponent,
) -> Result<(), PublishManifestError> {
    let key = (
        component.scope.package_id.clone(),
        component.scope.package_version.clone(),
        component.component_digest.clone(),
    );
    match closure.get(&key) {
        Some(existing) if existing != component => Err(PublishManifestError::new(
            PublishManifestErrorType::OperationDependency,
            format!("component dependency tuple {key:?} resolves more than one fact"),
        )),
        Some(_) => Ok(()),
        None => {
            closure.insert(key, component.clone());
            Ok(())
        }
    }
}

/// Resolve each base the package `(package, version)` pins, then its bases.
fn visit_base_dependencies(
    (package, version): (&str, &str),
    component_facts: &BTreeMap<(String, String), Vec<AdmittedComponent>>,
    package_manifests: &BTreeMap<String, wamn_schema_generator::PackageManifest>,
    rule: DependencyDigestRule,
    path: &mut Vec<(String, String)>,
    visited: &mut BTreeSet<(String, String)>,
    closure: &mut BTreeMap<(String, String, String), AdmittedComponent>,
) -> Result<(), PublishManifestError> {
    let coordinate = (package.to_owned(), version.to_owned());
    if path.contains(&coordinate) {
        return Err(PublishManifestError::new(
            PublishManifestErrorType::OperationDependency,
            "component dependency closure contains a cycle",
        ));
    }
    if !visited.insert(coordinate.clone()) {
        return Ok(());
    }
    let Some(manifest) = package_manifests
        .get(package)
        .filter(|manifest| manifest.package.version == version)
    else {
        return Ok(());
    };
    path.push(coordinate);
    for requirement in manifest.base_dependencies.values() {
        for base in resolve_base_dependency(requirement, component_facts, rule)? {
            insert_closure_component(closure, base)?;
        }
        visit_base_dependencies(
            (&requirement.package, &requirement.version),
            component_facts,
            package_manifests,
            rule,
            path,
            visited,
            closure,
        )?;
    }
    path.pop();
    Ok(())
}

/// The admitted facts of one pinned base: the one component under the pinned
/// digest, or under the development rule every component of the coordinate.
fn resolve_base_dependency<'a>(
    requirement: &wamn_schema_generator::BaseDependencyRequirement,
    component_facts: &'a BTreeMap<(String, String), Vec<AdmittedComponent>>,
    rule: DependencyDigestRule,
) -> Result<Vec<&'a AdmittedComponent>, PublishManifestError> {
    let Some(facts) =
        component_facts.get(&(requirement.package.clone(), requirement.version.clone()))
    else {
        return Err(PublishManifestError::new(
            PublishManifestErrorType::OperationDependency,
            format!(
                "component dependency {}@{} is absent from the effective release",
                requirement.package, requirement.version
            ),
        ));
    };
    let matches = facts
        .iter()
        .filter(|component| {
            !rule.matches_declared_digest() || component.component_digest == requirement.digest
        })
        .collect::<Vec<_>>();
    if matches.is_empty() || (rule.matches_declared_digest() && matches.len() > 1) {
        return Err(PublishManifestError::new(
            PublishManifestErrorType::OperationDependency,
            format!(
                "component dependency {}@{} digest {} resolves {} admitted facts, not one",
                requirement.package,
                requirement.version,
                requirement.digest,
                matches.len()
            ),
        ));
    }
    Ok(matches)
}

/// Project one release component.
///
/// The bases of a composed component run inside its own bytes, so the
/// manifest lists only the component itself. The component carries its
/// descriptor: `fact` with its `connections`.
pub(super) fn project_serving_component(
    fact: &AdmittedComponent,
    connections: Vec<wamn_catalog::ComponentConnection>,
) -> Result<ServingComponent, PublishManifestError> {
    ServingComponent::project(fact, connections).map_err(|error| {
        PublishManifestError::with_source(
            PublishManifestErrorType::Component,
            format!(
                "component {:?} does not project into one release component",
                fact.component
            ),
            error,
        )
    })
}

/// Refuse an anonymous attachment whose selected wiring can reach a registered
/// application operation or a transactional statement.
///
/// An anonymous request has no principal, so it cannot write (`wamn-emtx.22`).
///
/// The attachment itself cannot carry this fact for nested calls: reachability
/// exists only in the exact stored wiring plus its admitted component facts, so
/// release publish is the first boundary that can make the invalid composition
/// unrepresentable without adding another manifest field (`wamn-10yt.3.2`).
pub(super) fn validate_anonymous_wiring_closure(
    attachments: &BTreeMap<String, ServingAttachment>,
    target: &ReleaseWiringTarget,
    document: &WiringDocument,
    component_facts: &BTreeMap<String, AdmittedComponent>,
) -> Result<(), PublishManifestError> {
    let anonymous_attachments = attachments.iter().filter(|(_, attachment)| {
        attachment.package_id == target.package_id
            && attachment.target
                == wamn_catalog::AttachmentTarget::Wiring {
                    wiring_id: target.wiring_id.clone(),
                    wiring_version: target.wiring_version,
                }
            && wamn_catalog::parse_attachment_auth_policy(&attachment.auth_policy)
                .is_some_and(wamn_catalog::AttachmentAuthPolicy::has_no_caller)
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
            refuse_anonymous_reach(attachment_id, operation, &format!("node {node_id:?}"))?;
        }
    }
    Ok(())
}

/// Resolve the one admitted component that a route attachment calls.
///
/// A route names no interface version, so the component name and its export
/// select the fact. An anonymous route attachment cannot reach a registered
/// operation or a write, the same rule as an anonymous wiring.
pub(super) fn resolve_route_component<'a>(
    attachment_id: &str,
    attachment: &ServingAttachment,
    component: &str,
    operation: &str,
    facts: &'a [AdmittedComponent],
) -> Result<&'a AdmittedComponent, PublishManifestError> {
    let mut matches = facts
        .iter()
        .filter(|fact| fact.component == component && fact.operation(operation).is_some());
    let Some(fact) = matches.next() else {
        return Err(PublishManifestError::new(
            PublishManifestErrorType::Component,
            format!(
                "route attachment {attachment_id:?} names component {component:?} operation \
                 {operation:?}, which no admitted component of package {:?} exports",
                attachment.package_id
            ),
        ));
    };
    if matches.next().is_some() {
        return Err(PublishManifestError::new(
            PublishManifestErrorType::Component,
            format!(
                "route attachment {attachment_id:?} operation {operation:?} resolves more than \
                 one admitted component"
            ),
        ));
    }
    if wamn_catalog::parse_attachment_auth_policy(&attachment.auth_policy)
        .is_some_and(wamn_catalog::AttachmentAuthPolicy::has_no_caller)
    {
        let export = fact
            .operation(operation)
            .expect("the route component exports its operation");
        refuse_anonymous_reach(attachment_id, export, &format!("route {operation:?}"))?;
    }
    Ok(fact)
}

/// Refuse an anonymous attachment that reaches a registered operation, an
/// operation that begins a host transaction, or a transactional statement at
/// `site`.
fn refuse_anonymous_reach(
    attachment_id: &str,
    operation: &AdmittedComponentOperation,
    site: &str,
) -> Result<(), PublishManifestError> {
    if let Some(operation) = operation.registered_operation.as_deref() {
        return Err(PublishManifestError::new(
            PublishManifestErrorType::UnauthenticatedRegisteredOperation,
            format!(
                "attachment {attachment_id:?} reaches registered operation \
                 {operation:?} at {site}; set auth-policy modes = \
                 [{mode:?}]",
                mode = wamn_catalog::PAT_AUTHENTICATION_MODE,
            ),
        ));
    }
    if operation.host_transaction {
        return Err(PublishManifestError::new(
            PublishManifestErrorType::UnauthenticatedWrite,
            format!(
                "attachment {attachment_id:?} reaches an operation that begins a host \
                 transaction at {site}; set auth-policy modes = [{mode:?}]",
                mode = wamn_catalog::PAT_AUTHENTICATION_MODE,
            ),
        ));
    }
    if let Some(statement) = operation
        .statements
        .values()
        .find(|statement| statement.transactional)
    {
        return Err(PublishManifestError::new(
            PublishManifestErrorType::UnauthenticatedWrite,
            format!(
                "attachment {attachment_id:?} reaches transactional statement \
                 {:?} at {site}; set auth-policy modes = [{mode:?}]",
                statement.name,
                mode = wamn_catalog::PAT_AUTHENTICATION_MODE,
            ),
        ));
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
