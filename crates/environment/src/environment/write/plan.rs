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
pub(super) async fn release_values(
    document: &EnvironmentDocument,
    application: &wamn_catalog::ApplicationName,
    manifest: &wamn_catalog::ServingManifest,
    manifest_digest: &wamn_digest::Digest,
    bindings: &[super::super::bindings::Binding],
    integration_repository: &str,
    actor: &str,
    source: wamn_oci::component_artifact::component_artifact_source::ComponentArtifactSourceConfig,
) -> anyhow::Result<(Value, Vec<RoleName>)> {
    let seconds = document
        .revocation_seconds
        .context("deploy requires revocation_seconds from the environment input")?;
    let roles = release_chart::roles(
        manifest,
        &document.org,
        &document.project,
        &document.env,
        application,
        seconds,
        bindings,
        integration_repository,
    )?;
    let names = roles.iter().map(|role| role.name).collect();
    let selection = document
        .applications
        .get(application)
        .with_context(|| format!("application {application} is not declared"))?;
    let applications = std::collections::BTreeMap::from([(
        application.to_owned(),
        wamn_catalog::ApplicationDefinition {
            packages: manifest
                .release
                .packages
                .iter()
                .map(|package| package.package_id().to_owned())
                .collect(),
            connections: std::collections::BTreeMap::new(),
        },
    )]);
    let mut rendered = release_chart::values(&ValuesInput {
        org: document.org.clone(),
        project: document.project.clone(),
        env: document.env.clone(),
        manifest_digest: manifest_digest.to_owned(),
        route_host: selection.hostname.clone(),
        applications,
        roles,
        actor: actor.to_owned(),
    })?;
    let source =
        wamn_oci::component_artifact::component_artifact_source::ComponentArtifactSource::new(
            source,
        )?;
    let mut components = Vec::with_capacity(manifest.components.len());

    for component in &manifest.components {
        let image = source
            .image_reference(&component.descriptor.component_digest)
            .await?;
        components.push(serde_yaml::to_value(serde_json::json!({
            "name": release_chart::component_name(component), "image": image,
        }))?);
    }
    rendered["components"] = Value::Sequence(components);
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
