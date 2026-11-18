use super::{EnvironmentDocument, Mapping, RoleName, Value, ValuesInput, release_chart};
use anyhow::Context as _;

pub(in crate::environment) fn host_group(mut group: Mapping) -> anyhow::Result<Mapping> {
    // The upstream chart replaces a group's resources wholesale.
    // Fill omitted defaults before it receives a placement-specific override.
    if let Some(resources) = group.get_mut("resources") {
        let resources = resources
            .as_mapping_mut()
            .context("host resources are not a mapping")?;
        let mut defaults: Value = serde_yaml::from_str(include_str!(
            "../../../../../deploy/charts/environment/values.yaml"
        ))?;
        let Value::Mapping(defaults) =
            std::mem::take(&mut defaults["runtime-operator"]["runtime"]["resources"])
        else {
            anyhow::bail!("platform host resources are not a mapping");
        };
        for (name, value) in defaults {
            resources.entry(name).or_insert(value);
        }
    }
    group
        .entry("env".into())
        .or_insert_with(|| Value::Sequence(Vec::new()))
        .as_sequence_mut()
        .context("the platform host group's `env` is not a list")?;
    Ok(group)
}

/// Build release values from the document, artifact, policy, and host overlay.
pub(super) fn release_values(
    document: &EnvironmentDocument,
    application: &wamn_catalog::ApplicationName,
    package_manifests: &std::collections::BTreeMap<
        wamn_catalog::PackageName,
        wamn_catalog::manifest::PackageManifest,
    >,
    package_digest: &wamn_digest::Digest,
    bindings: &[super::super::bindings::Binding],
    integration_repository: &str,
    actor: &str,
    component_repository: &str,
) -> anyhow::Result<(Value, Vec<RoleName>)> {
    let seconds = document
        .revocation_seconds
        .context("deploy requires revocation_seconds from the environment input")?;
    let roles = release_chart::roles(
        package_manifests,
        package_digest,
        &document.org,
        &document.project,
        &document.env,
        application,
        seconds,
        bindings,
        integration_repository,
    )?;
    let names = roles.iter().map(|role| role.name).collect();
    let selection = &document.applications[application];
    let applications = std::collections::BTreeMap::from([(
        application.to_owned(),
        wamn_catalog::ApplicationDefinition {
            packages: package_manifests.keys().cloned().collect(),
            connections: std::collections::BTreeMap::new(),
        },
    )]);
    let mut rendered = release_chart::values(&ValuesInput {
        org: document.org.clone(),
        project: document.project.clone(),
        env: document.env.clone(),
        manifest_digest: *package_digest,
        route_host: selection.hostname.clone(),
        applications,
        roles,
        actor: actor.to_owned(),
    })?;
    let repository = wamn_catalog::parse_component_artifact_base(component_repository)?;
    let components = release_chart::published_components(package_manifests)
        .map(|(package, component)| {
            let name = release_chart::component_name(package, &component.descriptor.component);
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
    rendered["webClients"] = serde_yaml::to_value(
        selection
            .client
            .as_ref()
            .map(|client| (&selection.hostname, &client.package))
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>(),
    )?;
    rendered["webDigests"] = serde_yaml::to_value(
        selection
            .client
            .as_ref()
            .map(|client| (&selection.hostname, &client.web))
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>(),
    )?;
    Ok((rendered, names))
}
