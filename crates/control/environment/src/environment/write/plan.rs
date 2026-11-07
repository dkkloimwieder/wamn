use super::{
    EnvironmentDocument, HOST_CREDENTIALS, Mapping, RoleName, Value, ValuesInput, ensure,
    release_chart,
};
use anyhow::Context as _;

pub(super) fn host_group(
    mut group: Mapping,
    org: &str,
    project: &str,
    env: &str,
) -> anyhow::Result<Mapping> {
    // The upstream chart replaces a group's resources wholesale.
    // Fill omitted defaults before it receives a placement-specific override.
    if let Some(resources) = group.get_mut("resources") {
        let resources = resources
            .as_mapping_mut()
            .context("host resources are not a mapping")?;
        let mut defaults: Value = serde_yaml::from_str(include_str!(
            "../../../../../../deploy/platform/release/values.yaml"
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
    let variables = group
        .entry("env".into())
        .or_insert_with(|| Value::Sequence(Vec::new()))
        .as_sequence_mut()
        .context("the platform host group's `env` is not a list")?;
    for credential in HOST_CREDENTIALS {
        let Some(variable) = credential.host_variable else {
            continue;
        };
        ensure!(
            !variables
                .iter()
                .any(|entry| entry["name"].as_str() == Some(variable)),
            "the platform host group sets {variable}, which apply derives from the coordinate"
        );
        let secret =
            wamn_control_provision::workload_secret_name(credential.family, org, project, env);
        variables.push(serde_yaml::from_str(&format!(
            "{{name: {variable}, valueFrom: {{secretKeyRef: {{name: {secret}, key: url, optional: false}}}}}}"
        ))?);
    }
    let mut group = serde_json::to_value(group)?;
    let encryption = group["env"]
        .as_array()
        .and_then(|env| {
            env.iter()
                .find(|entry| entry["name"] == "WAMN_OAUTH_ENCRYPTION")
        })
        .and_then(|entry| entry["value"].as_str())
        .map(serde_json::from_str)
        .transpose()?
        .unwrap_or_else(|| serde_json::json!({"provider":"local"}));
    crate::platform::oauth::host_group(&mut group, &encryption)?;
    Ok(serde_yaml::from_value(serde_yaml::to_value(group)?)?)
}

/// Build release values from the document, artifact, policy, and host overlay.
#[expect(
    clippy::too_many_arguments,
    reason = "Release values combine document, artifact, policy, and chart inputs"
)]
pub(super) fn release_values(
    document: &EnvironmentDocument,
    application: &str,
    manifest: &wamn_catalog::ServingManifest,
    manifest_digest: &str,
    artifact_base: &str,
    drain_bound_seconds: u32,
    actor: &str,
    host_group: Mapping,
    clients: &std::collections::BTreeMap<String, String>,
) -> anyhow::Result<(Value, Vec<RoleName>)> {
    let roles = release_chart::roles(manifest, &document.org, &document.project, &document.env);
    let names = roles.iter().map(|role| role.name).collect();
    let selection = document
        .applications
        .get(application)
        .with_context(|| format!("application {application} is not declared"))?;
    let applications = std::collections::BTreeMap::from([(
        application.to_owned(),
        wamn_catalog::ApplicationDefinition {
            packages: selection.packages.clone(),
            connections: std::collections::BTreeMap::new(),
        },
    )]);
    let local_components = host_group
        .get("env")
        .and_then(Value::as_sequence)
        .into_iter()
        .flatten()
        .find(|entry| entry["name"] == "WAMN_COMPONENT_ARTIFACT_BASE")
        .and_then(|entry| entry["value"].as_str())
        .filter(|base| base.starts_with(&format!("{}/", crate::platform::resources::POD_REGISTRY)));
    let pod_artifact_base =
        local_components.map(|base| base.trim_end_matches("/components").to_owned() + "/releases");
    let mut rendered = release_chart::values(&ValuesInput {
        org: document.org.clone(),
        project: document.project.clone(),
        env: document.env.clone(),
        manifest_digest: manifest_digest.to_owned(),
        artifact_base: pod_artifact_base
            .as_deref()
            .unwrap_or(artifact_base)
            .to_owned(),
        route_host: selection.hostname.clone(),
        applications,
        roles,
        drain_bound_seconds,
        actor: actor.to_owned(),
        host_group,
    })?;
    if pod_artifact_base.is_some() {
        rendered["runtime-operator"]["runtime"]["hostGroups"][0]["extraArgs"]
            .as_sequence_mut()
            .context("application host arguments")?
            .push("--allow-insecure-registries".into());
    }
    rendered["webClients"] = serde_yaml::to_value(
        clients
            .get(&selection.hostname)
            .map(|client| (&selection.hostname, client))
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
