//! Exact component facts and the bases their packages pin.

use wamn_catalog::AdmittedComponentOperation;

use super::{
    AdmittedComponent, BTreeMap, BTreeSet, DependencyDigestRule, PublishManifestError,
    PublishManifestErrorType, ServingAttachment, ServingComponent,
};

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
