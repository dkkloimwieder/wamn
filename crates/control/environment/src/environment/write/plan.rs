use super::{
    EnvironmentDocument, HOST_CREDENTIALS, Mapping, RoleName, Value, ValuesInput, release_chart,
};
use anyhow::Context as _;

pub(in crate::environment) fn host_group(
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
            "../../../../../../deploy/platform/environment/values.yaml"
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
        anyhow::ensure!(
            !variables
                .iter()
                .any(|entry| entry["name"].as_str() == Some(variable)),
            "the platform host group sets {variable}, which apply derives from the coordinate"
        );
        variables.push(serde_yaml::from_str(&format!(
            "{{name: {variable}, value: /etc/wamn/postgres/{}.json}}",
            if credential.family == wamn_control_provision::WorkloadRoleFamily::App {
                "app".to_owned()
            } else {
                credential.family.component_stem()
            }
        ))?);
    }
    let mut group = serde_json::to_value(group)?;
    if let Some(volumes) = group["volumes"].as_array_mut() {
        volumes.retain(|volume| volume["name"] != "postgres-configuration");
    }
    if !group["volumes"].is_array() {
        group["volumes"] = serde_json::json!([]);
    }
    group["volumes"].as_array_mut().unwrap().push(serde_json::json!({"name":"postgres-configuration", "configMap":{"name":wamn_control_provision::project_env_secret_name(org, project, env)}}));
    if !group["volumeMounts"].is_array() {
        group["volumeMounts"] = serde_json::json!([]);
    }
    group["volumeMounts"].as_array_mut().unwrap().push(serde_json::json!({"name":"postgres-configuration", "mountPath":"/etc/wamn/postgres", "readOnly":true}));
    Ok(serde_yaml::from_value(serde_yaml::to_value(group)?)?)
}

/// Derive registry arguments from the platform host's environment variables.
pub(in crate::environment) fn registry_arguments(host_group: &Mapping) -> Vec<String> {
    host_group
        .get("env")
        .and_then(Value::as_sequence)
        .into_iter()
        .flatten()
        .filter_map(|variable| match variable["name"].as_str() {
            Some("WAMN_REGISTRY_AUTH_FILE") => variable["value"]
                .as_str()
                .map(|path| format!("--registry-auth-file={path}")),
            Some("WAMN_REGISTRY_TOKEN_METADATA") if variable["value"].as_str() == Some("true") => {
                Some("--registry-token-metadata".into())
            }
            _ => None,
        })
        .collect()
}

/// Build release values from the document, artifact, policy, and host overlay.
pub(super) fn release_values(
    document: &EnvironmentDocument,
    application: &str,
    manifest: &wamn_catalog::ServingManifest,
    manifest_digest: &str,
    artifact_base: &str,
    component_base: &str,
    actor: &str,
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
        artifact_base: artifact_base.to_owned(),
        route_host: selection.hostname.clone(),
        applications,
        roles,
        actor: actor.to_owned(),
        component_base: component_base.to_owned(),
    })?;
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
