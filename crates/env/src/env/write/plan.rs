use std::collections::BTreeMap;

use crate::app_chart::{
    binding::Binding,
    plan::{self as chart, Publication, PublishedOperation},
};
use wamn_release::publish_release::{deploy_runtime_schema, deployment_routes, route_contracts};

use super::{EnvDocument, Value, ValuesInput};

/// Build release values from the document, artifact, and host overlay.
#[expect(
    clippy::too_many_arguments,
    reason = "the release plan needs separate target, binding, and repository inputs"
)]
pub(super) fn release_values(
    document: &EnvDocument,
    platform_domain: &wamn_registry::platform_domain::PlatformDomain,
    app: &wamn_registry::AppName,
    package_manifests: &std::collections::BTreeMap<
        wamn_package::PackageName,
        wamn_component_policy::PackageManifest,
    >,
    package_digest: &wamn_digest::Digest,
    bindings: &[Binding],
    integration_repository: &str,
    component_repository: &str,
    authority_port: super::AuthorityPort,
) -> anyhow::Result<Value> {
    let contracts = route_contracts(package_manifests)?;
    let routes = deployment_routes(package_manifests, &contracts, app, package_digest)?;
    let schemas = package_manifests
        .iter()
        .map(|(id, package)| Ok((id, deploy_runtime_schema(package, package_manifests)?)))
        .collect::<anyhow::Result<_>>()?;
    let mut operations = BTreeMap::new();
    for ((package, operation), contract) in contracts {
        operations
            .entry(package)
            .or_insert_with(BTreeMap::new)
            .insert(
                operation,
                PublishedOperation {
                    type_: contract.type_,
                    idempotency: contract.idempotency,
                },
            );
    }
    let publication = Publication {
        routes,
        operations,
        schemas,
    };
    let route = chart::roles(
        package_manifests,
        &publication,
        &document.settings.org,
        &document.settings.project,
        &document.settings.env,
        app,
        document.revocation_seconds,
        bindings,
        integration_repository,
        |authority| Ok(authority_port(authority)?),
    )?;
    let selection = &document.applications[app];
    let apps = std::collections::BTreeMap::from([(
        app.to_owned(),
        wamn_registry::AppDefinition {
            packages: package_manifests.keys().cloned().collect(),
            connections: std::collections::BTreeMap::new(),
        },
    )]);
    let mut rendered = chart::values(&ValuesInput {
        org: document.settings.org.clone(),
        project: document.settings.project.clone(),
        env: document.settings.env.clone(),
        manifest_digest: *package_digest,
        route_label: &selection.label,
        platform_domain,
        client: selection.client.as_ref(),
        apps,
        route,
    })?;
    let repository = wamn_oci_reference::parse_component_artifact_base(component_repository)?;
    let components = chart::published_components(package_manifests)
        .map(|(package, component)| {
            let name = chart::component_name(package, &component.descriptor.component);
            let image = format!(
                "{}/{}@{}",
                repository.registry(),
                repository.repository(),
                component.image_digest
            );
            serde_yaml::to_value(serde_json::json!({"name": name, "image": image}))
        })
        .collect::<Result<Vec<_>, _>>()?;
    rendered["components"]
        .as_sequence_mut()
        .expect("release values contain a component list")
        .extend(components);
    Ok(rendered)
}
