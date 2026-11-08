use super::{
    AdmittedComponent, AttachmentTarget, AuthoritativeEnvironmentPolicy, BTreeMap, BTreeSet,
    BareSchemaName, ComposedApplication, DependencyDigestRule, PackageCoordinate,
    ProjectedEnvironmentPolicy, PublishManifestError, PublishManifestErrorType,
    PublishReleaseManifest, PublishedRelease, ReleaseComponents, ReleaseScope, RouteContracts,
    ServingComponent, ServingManifest, ServingRoute, ensure, project_serving_component,
    resolve_component_dependency_closure, resolve_route_component,
    validate_attachment_definition_hashes,
};
use anyhow::Context as _;

pub(super) fn projected_environment_policy_sql(run_schema: &BareSchemaName) -> String {
    format!(
        "SELECT expected_environment, source_policy_org, source_policy_hash \
           FROM {}.environment_policies WHERE env_key = $1",
        run_schema.quoted()
    )
}

/// Parse one exact `PACKAGE_ID@PACKAGE_VERSION` package coordinate.
pub fn parse_package(value: &str) -> Result<PackageCoordinate, String> {
    let (package_id, package_version) = value
        .rsplit_once('@')
        .ok_or_else(|| "expected PACKAGE_ID@PACKAGE_VERSION".to_owned())?;
    PackageCoordinate::new(package_id, package_version).map_err(|error| error.to_string())
}

/// The digests of an admitted application and of the members it embeds.
pub(super) fn application_digests(
    application: &AdmittedComponent,
    application_bytes: &[u8],
) -> anyhow::Result<BTreeSet<String>> {
    let mut digests = BTreeSet::from([application.component_digest.as_str().to_owned()]);
    for payload in wasmparser::Parser::new(0).parse_all(application_bytes) {
        if let wasmparser::Payload::ComponentSection {
            unchecked_range, ..
        } = payload?
        {
            let member = &application_bytes
                [usize::try_from(unchecked_range.start)?..usize::try_from(unchecked_range.end)?];
            digests.insert(wamn_engine::component_admission::component_digest(member));
        }
    }
    Ok(digests)
}

/// Whether one application, with the members it embeds, serves every release route.
///
/// Publication requires exactly one declared application member to pass this test.
pub(super) fn covers_routes(
    published: &PublishedRelease,
    application: &AdmittedComponent,
    application_bytes: &[u8],
) -> anyhow::Result<bool> {
    let digests = application_digests(application, application_bytes)?;
    Ok(published.manifest.routes.iter().all(|route| {
        published.manifest.components.iter().any(|component| {
            component.package_id == route.package_id
                && component.descriptor.component == route.component
                && digests.contains(component.descriptor.component_digest.as_str())
        })
    }))
}

/// Record the exact composed entry before publication hashes the release.
///
/// The record names the released application components embedded in `entry`.
/// The manifest retains their original digests for SQL and dependency authority.
pub fn record_composed_entry(
    published: &mut PublishedRelease,
    entry: &ComposedApplication,
    reference: &str,
) -> anyhow::Result<()> {
    let members: BTreeSet<_> = published
        .manifest
        .components
        .iter()
        .filter(|component| {
            entry
                .application_digests
                .contains(&component.descriptor.component_digest)
        })
        .map(|component| wamn_catalog::ServingEntryMember {
            package_id: component.package_id.clone(),
            component: component.descriptor.component.clone(),
            digest: component.descriptor.component_digest.clone(),
        })
        .collect();
    ensure!(
        members.contains(&entry.application),
        "composed application is outside the released members"
    );
    let mut manifest = published.manifest.clone();
    manifest.composed_entry = Some(wamn_catalog::ServingComposedEntry {
        reference: reference.to_owned(),
        route_digest: entry.route_digest.clone(),
        application: entry.application.clone(),
        members,
    });
    let canonical_bytes = manifest.canonical_bytes();
    let (manifest, digest) = ServingManifest::from_canonical_bytes(&canonical_bytes)?;
    *published = PublishedRelease {
        manifest,
        digest,
        canonical_bytes,
    };
    Ok(())
}

pub(crate) fn verify_projected_environment_policy(
    projected: Option<&ProjectedEnvironmentPolicy>,
    source_policy: &AuthoritativeEnvironmentPolicy,
    scope: &ReleaseScope,
    run_schema: &BareSchemaName,
) -> Result<(), PublishManifestError> {
    let Some(projected) = projected else {
        return Err(environment_policy_absent(scope, run_schema));
    };
    verify_environment_name(&projected.expected_environment, scope)?;
    if projected.source_policy_org.as_deref() != Some(source_policy.source_policy_org.as_ref())
        || projected.source_policy_hash.as_deref()
            != Some(source_policy.source_policy_hash.as_ref())
    {
        return Err(PublishManifestError::new(
            PublishManifestErrorType::EnvironmentPolicySourceMismatch,
            format!(
                "environment {:?} policy source differs: projected-org={:?}, authoritative-org={:?}, projected-hash={:?}, authoritative-hash={:?}; rerun the verification policy projection",
                scope.environment,
                projected.source_policy_org.as_deref().unwrap_or("<absent>"),
                source_policy.source_policy_org,
                projected
                    .source_policy_hash
                    .as_deref()
                    .unwrap_or("<absent>"),
                source_policy.source_policy_hash,
            ),
        ));
    }
    Ok(())
}

pub(super) fn environment_policy_absent(
    scope: &ReleaseScope,
    run_schema: &BareSchemaName,
) -> PublishManifestError {
    PublishManifestError::new(
        PublishManifestErrorType::EnvironmentPolicyAbsent,
        format!(
            "tenant {:?} has no row in {}.environment_policies; run `wamn-ctl env apply`",
            scope.env_key,
            run_schema.as_str()
        ),
    )
}

pub(super) fn verify_environment_name(
    expected_environment: &str,
    scope: &ReleaseScope,
) -> Result<(), PublishManifestError> {
    if expected_environment != scope.environment {
        return Err(PublishManifestError::new(
            PublishManifestErrorType::EnvironmentPolicyMismatch,
            format!(
                "release environment {:?} differs from provisioned environment {:?}",
                scope.environment, expected_environment
            ),
        ));
    }
    Ok(())
}

pub(super) fn validate_request(
    request: &PublishReleaseManifest<'_>,
) -> Result<(), PublishManifestError> {
    if request.environment.is_empty() {
        return Err(PublishManifestError::new(
            PublishManifestErrorType::Release,
            "environment is required",
        ));
    }
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
    validate_attachment_definition_hashes(request.attachments)?;
    Ok(())
}

/// Derive the one schema that an application's SQL resolves against.
pub(crate) fn application_runtime_schema(
    manifest: &wamn_catalog::manifest::PackageManifest,
    manifests: &BTreeMap<String, wamn_catalog::manifest::PackageManifest>,
    components: &BTreeSet<ServingComponent>,
) -> anyhow::Result<Option<String>> {
    let member_digests = components
        .iter()
        .filter(|component| component.package_id == manifest.package.id)
        .flat_map(|component| component.descriptor.statement_instances.values())
        .collect::<BTreeSet<_>>();
    let mut members = vec![manifest];
    for dependency in manifest.base_dependencies.values() {
        let composed = member_digests.contains(&dependency.digest)
            || components.iter().any(|component| {
                component.package_id == dependency.package
                    && member_digests.contains(&component.descriptor.component_digest)
            });
        if composed {
            members.push(
                manifests
                    .get(&dependency.package)
                    .with_context(|| format!("missing base manifest {}", dependency.package))?,
            );
        }
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

/// Select applications whose admitted component includes the integration base.
pub fn composes_integration(
    manifest: &wamn_catalog::manifest::PackageManifest,
    components: &BTreeSet<ServingComponent>,
) -> bool {
    manifest.base_dependencies.values().any(|dependency| {
        dependency.package == "wamn_integration"
            && components.iter().any(|application| {
                application.package_id == manifest.package.id
                    && application
                        .descriptor
                        .statement_instances
                        .values()
                        .any(|digest| {
                            digest == &dependency.digest
                                || components.iter().any(|base| {
                                    base.package_id == dependency.package
                                        && &base.descriptor.component_digest == digest
                                })
                        })
            })
    })
}

/// Resolve the declared dependency permissions of an integration application.
/// Publication resolves them against the released package manifests.
pub fn application_dependency_permissions(
    manifest: &wamn_catalog::manifest::PackageManifest,
    manifests: &BTreeMap<&str, &wamn_catalog::manifest::PackageManifest>,
) -> anyhow::Result<BTreeSet<String>> {
    let mut permissions = BTreeSet::new();
    for dependency in manifest.base_dependencies.values() {
        for operation in &dependency.operations {
            let base = manifests
                .get(dependency.package.as_str())
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
                    wamn_catalog::manifest::canonical_operation_identity(&base.package, operation)?;
                permissions.insert(
                    canonical
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
    manifests: &BTreeMap<String, wamn_catalog::manifest::PackageManifest>,
) -> Result<BTreeSet<ServingComponent>, PublishManifestError> {
    let dependency_manifests = manifests
        .iter()
        .map(|(id, manifest)| (id.as_str(), manifest))
        .collect();
    let schemas = manifests
        .iter()
        .map(|(id, manifest)| {
            if composes_integration(manifest, &components) {
                application_dependency_permissions(manifest, &dependency_manifests)?;
            }
            application_runtime_schema(manifest, manifests, &components)
                .map(|schema| (id.clone(), schema))
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

/// The admitted components of the opened package artifacts, each from its
/// descriptor layer, in the scope of `env_key`.
pub(super) fn project_components(
    components: &ReleaseComponents,
) -> Result<BTreeSet<ServingComponent>, PublishManifestError> {
    components
        .facts
        .values()
        .flatten()
        .map(|fact| {
            components.with_statement_instances(project_serving_component(
                fact,
                components.connections(fact)?,
            )?)
        })
        .collect()
}

/// Project operation contracts for released components and route attachments.
///
/// The kind comes from the generated contract of the operation, or from the
/// published manifest a promotion copies.
pub(super) fn project_routes(
    request: &PublishReleaseManifest<'_>,
    route_contracts: &RouteContracts,
    component_facts: &ReleaseComponents,
    package_manifests: &BTreeMap<String, wamn_catalog::manifest::PackageManifest>,
    components: &mut BTreeSet<ServingComponent>,
) -> Result<BTreeSet<ServingRoute>, PublishManifestError> {
    let rule = DependencyDigestRule::for_environment(request.environment_is_disposable);
    let mut routes = BTreeSet::new();
    for (attachment_id, attachment) in request.attachments {
        let AttachmentTarget::Route {
            component,
            operation,
        } = &attachment.target;
        let package = request
            .packages
            .iter()
            .find(|package| package.package_id() == attachment.package_id)
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
                        "route operation {operation:?} of package {:?} has no generated contract kind; regenerate the package evidence",
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
        components.insert(
            component_facts.with_statement_instances(project_serving_component(
                fact,
                component_facts.connections(fact)?,
            )?)?,
        );
        routes.insert(ServingRoute {
            package_id: attachment.package_id.clone(),
            component: component.clone(),
            operation: operation.clone(),
            type_: contract.kind,
            reads: contract.reads.clone(),
            revision: contract.revision.clone(),
            idempotency: contract.idempotency.clone(),
            canonicalization: contract.canonicalization.clone(),
        });
    }
    // Workflow nodes also use these contracts to begin request element transactions and
    // claim their keys, for private and registered commands alike. An HTTP
    // attachment is not required to call an operation.
    for component in components.iter() {
        for operation in component.operations.keys() {
            let Some(contract) =
                route_contracts.get(&(component.package_id.clone(), operation.clone()))
            else {
                continue;
            };
            if contract.kind == wamn_catalog::OperationType::EventHandler {
                continue;
            }
            routes.insert(ServingRoute {
                package_id: component.package_id.clone(),
                component: component.descriptor.component.clone(),
                operation: operation.clone(),
                type_: contract.kind,
                reads: contract.reads.clone(),
                revision: contract.revision.clone(),
                idempotency: contract.idempotency.clone(),
                canonicalization: contract.canonicalization.clone(),
            });
        }
    }
    Ok(routes)
}
