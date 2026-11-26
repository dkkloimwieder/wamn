use super::{
    AttachmentTarget, BTreeMap, BTreeSet, DependencyDigestRule, PackageCoordinate,
    PublishManifestError, PublishManifestErrorType, PublishReleaseManifest, ReleaseComponents,
    RouteContracts, ServingComponent, ServingRoute, ensure, resolve_component_dependency_closure,
    resolve_route_component,
};
use anyhow::Context as _;

/// Parse one exact `PACKAGE_ID@PACKAGE_VERSION` package coordinate.
pub fn parse_package(value: &str) -> Result<PackageCoordinate, String> {
    let (package_id, package_version) = value
        .rsplit_once('@')
        .ok_or_else(|| "expected PACKAGE_ID@PACKAGE_VERSION".to_owned())?;
    PackageCoordinate::new(
        package_id
            .parse()
            .map_err(|error: wamn_package::ParsePackageNameError| error.to_string())?,
        package_version
            .parse::<wamn_package::package_version::PackageVersion>()
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

pub(super) fn validate_request(
    request: &PublishReleaseManifest<'_>,
) -> Result<(), PublishManifestError> {
    if request.packages.is_empty() {
        return Err(PublishManifestError::new(
            PublishManifestErrorType::Release,
            "an effective release requires at least one exact package pair",
        ));
    }
    let mut ids = BTreeSet::new();
    if !request
        .packages
        .iter()
        .all(|package| ids.insert(package.package_id()))
    {
        return Err(PublishManifestError::new(
            PublishManifestErrorType::Release,
            "an effective release cannot contain two versions of one package",
        ));
    }
    Ok(())
}

/// Derive the one schema that an application's SQL resolves against.
pub(crate) fn app_runtime_schema(
    manifest: &wamn_component_policy::PackageManifest,
    manifests: &BTreeMap<wamn_package::PackageName, wamn_component_policy::PackageManifest>,
    components: &BTreeSet<ServingComponent>,
) -> anyhow::Result<Option<String>> {
    runtime_schema(manifest, manifests, |dependency| {
        has_base_exports(
            dependency,
            components
                .iter()
                .filter(|component| component.package_id == manifest.package.id)
                .flat_map(|component| component.descriptor.operations.keys()),
        )
    })
}

fn has_base_exports<'a>(
    dependency: &wamn_package::manifest::BaseDependencyRequirement,
    mut operations: impl Iterator<Item = &'a wamn_package::canonical_name::CanonicalName>,
) -> bool {
    let prefix = format!("{}:", dependency.package.as_str().replace('_', "-"));
    let version = format!("@{}", dependency.version);
    operations.any(|name| name.as_str().starts_with(&prefix) && name.as_str().ends_with(&version))
}

fn runtime_schema<'a>(
    manifest: &'a wamn_component_policy::PackageManifest,
    manifests: &'a BTreeMap<wamn_package::PackageName, wamn_component_policy::PackageManifest>,
    composes: impl Fn(&wamn_package::manifest::BaseDependencyRequirement) -> bool,
) -> anyhow::Result<Option<String>> {
    let mut members = vec![manifest];
    for dependency in manifest
        .base_dependencies
        .values()
        .filter(|dependency| composes(dependency))
    {
        members.push(
            manifests
                .get(&dependency.package)
                .with_context(|| format!("missing base manifest {}", dependency.package))?,
        );
    }
    let schemas = members
        .into_iter()
        .flat_map(|manifest| {
            manifest
                .models
                .values()
                .map(|model| model.schema.as_str())
                .chain(
                    manifest
                        .internal_relations
                        .values()
                        .map(|relation| relation.schema.as_str()),
                )
                .chain(manifest.custom_operations.values().flat_map(|operation| {
                    operation
                        .relations
                        .iter()
                        .map(|relation| relation.schema.as_str())
                }))
        })
        .collect::<BTreeSet<_>>();
    match schemas.into_iter().collect::<Vec<_>>().as_slice() {
        [] if manifest.declares_no_sql() => Ok(None),
        [schema] => Ok(Some((*schema).to_owned())),
        _ => anyhow::bail!(
            "one runtime schema is required for package {}",
            manifest.package.id
        ),
    }
}

fn deployed_exports(
    manifest: &wamn_component_policy::PackageManifest,
) -> impl Iterator<Item = &wamn_package::canonical_name::CanonicalName> {
    manifest
        .deploy
        .iter()
        .flat_map(|deploy| deploy.components.values())
        .flat_map(|component| component.descriptor.operations.keys())
}

/// Return whether the deployed package composes the integration base.
pub fn composes_integration(manifest: &wamn_component_policy::PackageManifest) -> bool {
    manifest.base_dependencies.values().any(|dependency| {
        dependency.package.as_str() == "wamn_integration"
            && has_base_exports(dependency, deployed_exports(manifest))
    })
}

/// Derive the workload schema from its admitted exports and the closure's SQL.
pub fn deploy_runtime_schema(
    manifest: &wamn_component_policy::PackageManifest,
    manifests: &BTreeMap<wamn_package::PackageName, wamn_component_policy::PackageManifest>,
) -> anyhow::Result<Option<String>> {
    runtime_schema(manifest, manifests, |dependency| {
        has_base_exports(dependency, deployed_exports(manifest))
    })
}

/// Resolve the declared dependency permissions of an integration application.
/// Publication resolves them against the released package manifests.
pub fn app_dependency_permissions(
    manifest: &wamn_component_policy::PackageManifest,
    manifests: &BTreeMap<&wamn_package::PackageName, &wamn_component_policy::PackageManifest>,
) -> anyhow::Result<BTreeSet<String>> {
    let mut permissions = BTreeSet::new();
    for dependency in manifest.base_dependencies.values() {
        for operation in &dependency.operations {
            let base = manifests
                .get(&dependency.package)
                .with_context(|| format!("missing dependency manifest {}", dependency.package))?;
            ensure!(
                base.package.version == dependency.version,
                "dependency {} version differs from the release",
                dependency.package
            );
            let registered = base
                .custom_operations
                .get(operation)
                .map(|operation| operation.permission().is_some())
                .or_else(|| {
                    base.models
                        .values()
                        .flat_map(|model| model.operations.values())
                        .find(|declared| declared.permission == *operation)
                        .map(|_| true)
                })
                .with_context(|| {
                    format!(
                        "declared dependency {}:{operation} is absent from the release",
                        dependency.package
                    )
                })?;
            if registered {
                let canonical =
                    wamn_package::manifest::canonical_operation_identity(&base.package, operation)?;
                permissions.insert(
                    canonical
                        .as_str()
                        .split_once('@')
                        .expect("canonical operation has a version")
                        .0
                        .to_owned(),
                );
            }
        }
    }
    Ok(permissions)
}

pub(super) fn pin_runtime_schemas(
    components: BTreeSet<ServingComponent>,
    manifests: &BTreeMap<wamn_package::PackageName, wamn_component_policy::PackageManifest>,
) -> Result<BTreeSet<ServingComponent>, PublishManifestError> {
    let dependency_manifests = manifests.iter().collect();
    let schemas = manifests
        .iter()
        .map(|(id, manifest)| {
            if manifest.base_dependencies.values().any(|dependency| {
                dependency.package.as_str() == "wamn_integration"
                    && has_base_exports(
                        dependency,
                        components
                            .iter()
                            .filter(|component| &component.package_id == id)
                            .flat_map(|component| component.descriptor.operations.keys()),
                    )
            }) {
                app_dependency_permissions(manifest, &dependency_manifests)?;
            }
            app_runtime_schema(manifest, manifests, &components).map(|schema| (id.clone(), schema))
        })
        .collect::<anyhow::Result<BTreeMap<_, _>>>()
        .map_err(|error| {
            PublishManifestError::new(PublishManifestErrorType::PackageManifest, error.to_string())
        })?;
    Ok(components
        .into_iter()
        .map(|mut component| {
            component.runtime_schema = schemas.get(&component.package_id).cloned().flatten();
            component
        })
        .collect())
}

/// Project admitted components into the edge bundle format.
pub(super) fn project_components(
    components: &ReleaseComponents,
) -> Result<BTreeSet<ServingComponent>, PublishManifestError> {
    components
        .facts
        .values()
        .flatten()
        .map(|fact| {
            Ok(ServingComponent::project(
                fact,
                components.connections(fact)?,
            ))
        })
        .collect()
}

/// Project operation contracts for released components and route attachments.
///
/// The type comes from the generated contract of the operation, or from the
/// published manifest a promotion copies.
pub(super) fn project_routes(
    request: &PublishReleaseManifest<'_>,
    route_contracts: &RouteContracts,
    component_facts: &ReleaseComponents,
    package_manifests: &BTreeMap<wamn_package::PackageName, wamn_component_policy::PackageManifest>,
    components: &mut BTreeSet<ServingComponent>,
) -> Result<BTreeSet<ServingRoute>, PublishManifestError> {
    let rule = DependencyDigestRule::for_env(request.env_is_disposable);
    let mut routes = BTreeSet::new();
    for (attachment_id, attachment) in request.attachments {
        let AttachmentTarget::Route {
            component,
            operation,
        } = &attachment.target;
        let package = request
            .packages
            .iter()
            .find(|package| package.package_id() == &attachment.package_id)
            .ok_or_else(|| {
                PublishManifestError::new(
                    PublishManifestErrorType::Component,
                    format!(
                        "route attachment {attachment_id:?} names package {:?} outside the effective release membership",
                        attachment.package_id
                    ),
                )
            })?;
        let facts = component_facts
            .facts
            .get(&(
                package.package_id().to_owned(),
                package.package_version().to_owned(),
            ))
            .map_or(&[][..], Vec::as_slice);
        let fact = resolve_route_component(attachment_id, attachment, component, operation, facts)?;
        let contract = route_contracts
            .get(&(attachment.package_id.clone(), operation.clone()))
            .ok_or_else(|| {
                PublishManifestError::new(
                    PublishManifestErrorType::PackageManifest,
                    format!(
                        "route operation {operation:?} of package {:?} has no generated contract type; regenerate the package evidence",
                        attachment.package_id
                    ),
                )
            })?;
        let roots = BTreeMap::from([(operation.clone(), fact.clone())]);
        resolve_component_dependency_closure(
            &roots,
            &component_facts.facts,
            package_manifests,
            rule,
        )?;
        components.insert(ServingComponent::project(
            fact,
            component_facts.connections(fact)?,
        ));
        routes.insert(ServingRoute {
            package_id: attachment.package_id.clone(),
            component: component.clone(),
            operation: operation.clone(),
            type_: contract.type_,
            reads: contract.reads.clone(),
            revision: contract.revision.clone(),
            idempotency: contract.idempotency.clone(),
        });
    }
    // Workflow nodes also use these contracts to begin request element transactions and
    // claim their keys, for private and registered commands alike. An HTTP
    // attachment is not required to call an operation.
    for component in components.iter() {
        for operation in component.descriptor.operations.keys() {
            let Some(contract) =
                route_contracts.get(&(component.package_id.clone(), operation.clone()))
            else {
                continue;
            };
            if contract.type_ == wamn_package::OperationType::EventHandler {
                continue;
            }
            routes.insert(ServingRoute {
                package_id: component.package_id.clone(),
                component: component.descriptor.component.clone(),
                operation: operation.clone(),
                type_: contract.type_,
                reads: contract.reads.clone(),
                revision: contract.revision.clone(),
                idempotency: contract.idempotency.clone(),
            });
        }
    }
    Ok(routes)
}
