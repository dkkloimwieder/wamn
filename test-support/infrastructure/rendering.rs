//! Typed YAML rendering for temporary application environments.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use wamn_control_provision::{workload_role::WorkloadRoleFamily, workload_secret_name};

/// Event coordinates declared by the environment owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventIdentity {
    pub org: String,
    pub project: String,
    pub environment: String,
}

/// A provisioned role and the Secret selected for this test run.
#[derive(Debug, Clone)]
pub struct HostRoleSecret {
    pub family: WorkloadRoleFamily,
    pub name: String,
}

/// Identity declared by the selected application's host overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostIdentity {
    pub org: String,
    pub project: String,
    pub schema: String,
}

/// Inputs for the base host values and the selected application's overlay.
#[derive(Debug, Clone)]
pub struct HostValuesInput {
    pub namespace: String,
    pub host_tag: String,
    pub replicas: u32,
    pub component_artifact_base: String,
    pub release_artifact_base: String,
    pub manifest_digest: String,
    pub nats_url: String,
    pub event: EventIdentity,
    pub guest_secret_name: String,
    pub role_secrets: Vec<HostRoleSecret>,
    pub object_store_secret_name: Option<String>,
}

/// Host values in Helm's required application order.
#[derive(Debug)]
pub struct RenderedHostValues {
    pub base: String,
    pub overlay: String,
}

/// All identity claims consumed by the HTTP component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpClaims {
    #[serde(rename = "wamn.tenant")]
    pub tenant: String,
    #[serde(rename = "wamn.catalog")]
    pub catalog: String,
    #[serde(rename = "wamn.environment")]
    pub environment: String,
    #[serde(rename = "wamn.project")]
    pub project: String,
    #[serde(rename = "wamn.schema")]
    pub schema: String,
}

/// Inputs for one HTTP workload and its Service.
#[derive(Debug, Clone)]
pub struct HttpWorkloadInput {
    pub namespace: String,
    pub image: String,
    pub route_host: String,
    pub claims: HttpClaims,
}

/// Inputs for the materializer's existing workload template.
#[derive(Debug, Clone)]
pub struct MaterializerInput {
    pub workload: String,
    pub namespace: String,
    pub image: String,
    pub tenant: String,
    pub event: EventIdentity,
    pub event_stream: String,
    pub fetch_ms: u64,
    pub sweep_ms: u64,
}

/// Render host values without reading process state or opening files.
pub fn render_host_values(
    base_yaml: &str,
    overlay_yaml: &str,
    input: &HostValuesInput,
) -> anyhow::Result<RenderedHostValues> {
    let mut base: HostValues = serde_yaml::from_str(base_yaml).context("parse host base values")?;
    let mut overlay: HostValues =
        serde_yaml::from_str(overlay_yaml).context("parse host overlay values")?;
    base.runtime
        .image
        .as_mut()
        .context("host base runtime.image is missing")?
        .tag = input.host_tag.clone();
    let base_group = one_mut(&mut base.runtime.host_groups, "host base hostGroups")?;
    base_group.namespace = input.namespace.clone();
    base_group.replicas = input.replicas;
    let group = one_mut(&mut overlay.runtime.host_groups, "host overlay hostGroups")?;
    group.namespace = input.namespace.clone();
    group.replicas = input.replicas;
    unique_env(&group.env)?;
    let guest = env_mut(&mut group.env, "WAMN_PG_URL")?;
    guest
        .value_from
        .as_mut()
        .context("WAMN_PG_URL requires a Secret reference")?
        .secret_key_ref
        .name = input.guest_secret_name.clone();
    let mut families = BTreeSet::new();
    for secret in &input.role_secrets {
        let expected = workload_secret_name(
            secret.family,
            &input.event.org,
            &input.event.project,
            &input.event.environment,
        );
        ensure!(
            families.insert(expected.clone()),
            "host role Secret is declared twice: {expected}"
        );
        let mut references = group
            .env
            .iter_mut()
            .filter_map(|entry| entry.value_from.as_mut())
            .map(|source| &mut source.secret_key_ref)
            .filter(|reference| reference.name == expected);
        let reference = references
            .next()
            .with_context(|| format!("host overlay is missing role Secret {expected}"))?;
        ensure!(
            references.next().is_none(),
            "host overlay repeats role Secret {expected}"
        );
        reference.name = secret.name.clone();
    }
    let mut object_volumes = group
        .volumes
        .iter_mut()
        .filter(|volume| volume.name == "connection-credentials");
    let object_volume = object_volumes.next();
    ensure!(
        object_volumes.next().is_none(),
        "host overlay repeats connection-credentials volume"
    );
    match (&input.object_store_secret_name, object_volume) {
        (Some(name), Some(volume)) => {
            volume
                .secret
                .as_mut()
                .context("connection-credentials requires a Secret volume")?
                .secret_name = name.clone()
        }
        (None, None) => {}
        (None, Some(_)) => {
            bail!("host overlay mounts object-store credentials but none were declared")
        }
        (Some(_), None) => bail!("host overlay has no declared object-store credentials volume"),
    }
    for (name, value) in [
        (
            "WAMN_COMPONENT_ARTIFACT_BASE",
            input.component_artifact_base.as_str(),
        ),
        ("WAMN_EVT_NATS_URL", input.nats_url.as_str()),
        ("WAMN_EVT_ORG", input.event.org.as_str()),
        ("WAMN_EVT_PROJECT", input.event.project.as_str()),
        ("WAMN_EVT_ENV", input.event.environment.as_str()),
    ] {
        let variable = env_mut(&mut group.env, name)?;
        ensure!(
            variable.value_from.is_none(),
            "{name} must be a declared value in the app overlay"
        );
        variable.value = Some(value.to_owned());
    }
    env_mut(&mut group.env, "WAMN_WASMTIME_CACHE_DIR")?;
    let telemetry_index = group
        .env
        .iter()
        .position(|entry| entry.name == "WAMN_WASMTIME_CACHE_DIR")
        .context("host cache variable is missing")?
        + 1;
    for (offset, (name, value)) in [
        (
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            "http://otel-collector.wamn-system.svc.cluster.local:4317",
        ),
        ("OTEL_BSP_SCHEDULE_DELAY", "1"),
        ("OTEL_BSP_MAX_EXPORT_BATCH_SIZE", "1"),
    ]
    .into_iter()
    .enumerate()
    {
        ensure!(
            !group.env.iter().any(|entry| entry.name == name),
            "host overlay already declares {name}"
        );
        group.env.insert(
            telemetry_index + offset,
            EnvVar {
                name: name.to_owned(),
                value: Some(value.to_owned()),
                value_from: None,
                extra: BTreeMap::new(),
            },
        );
    }
    replace_arg(
        &mut group.extra_args,
        "--release-artifact-base",
        &input.release_artifact_base,
    )?;
    replace_arg(
        &mut group.extra_args,
        "--release-manifest-digest",
        &input.manifest_digest,
    )?;
    ensure!(
        !group
            .extra_args
            .iter()
            .any(|arg| arg == "--allow-insecure-registries"),
        "host overlay already enables insecure registries"
    );
    group
        .extra_args
        .push("--allow-insecure-registries".to_owned());
    Ok(RenderedHostValues {
        base: serde_yaml::to_string(&base).context("serialize host base values")?,
        overlay: serde_yaml::to_string(&overlay).context("serialize host overlay values")?,
    })
}

/// Check the overlay's own identity without changing it.
pub fn assert_rendered_identity(rendered: &str, expected: &HostIdentity) -> anyhow::Result<()> {
    let values: HostValues =
        serde_yaml::from_str(rendered).context("parse rendered host identity")?;
    ensure!(
        values.runtime.host_groups.len() == 1,
        "rendered host identity requires one host group"
    );
    let group = &values.runtime.host_groups[0];
    for (name, declared) in [
        ("WAMN_ORG", expected.org.as_str()),
        ("WAMN_PROJECT", expected.project.as_str()),
        ("WAMN_SCHEMA", expected.schema.as_str()),
    ] {
        let mut entries = group.env.iter().filter(|entry| entry.name == name);
        let entry = entries
            .next()
            .with_context(|| format!("rendered file declares no {name}"))?;
        ensure!(
            entries.next().is_none(),
            "rendered file declares {name} more than once"
        );
        ensure!(
            entry.value_from.is_none(),
            "rendered file must declare {name} as a value"
        );
        let value = entry
            .value
            .as_deref()
            .with_context(|| format!("rendered file declares no value for {name}"))?;
        ensure!(
            value == declared,
            "rendered file claims {name}={value}, but this application declares {declared}"
        );
    }
    Ok(())
}

/// Render both HTTP documents with one route and all five claims.
pub fn render_http_workload(template: &str, input: &HttpWorkloadInput) -> anyhow::Result<String> {
    let mut documents = serde_yaml::Deserializer::from_str(template)
        .map(HttpDocument::deserialize)
        .collect::<Result<Vec<_>, _>>()
        .context("parse HTTP workload documents")?;
    ensure!(
        documents.len() == 2,
        "HTTP template must contain one Service and one WorkloadDeployment"
    );
    let mut services = 0;
    let mut workloads = 0;
    for document in &mut documents {
        match document {
            HttpDocument::Service(service) => {
                services += 1;
                service.metadata.namespace = input.namespace.clone();
            }
            HttpDocument::WorkloadDeployment(workload) => {
                workloads += 1;
                workload.metadata.namespace = input.namespace.clone();
                let spec = &mut workload.spec.template.spec;
                spec.environment = input.namespace.clone();
                let component = one_mut(&mut spec.components, "HTTP components")?;
                component.image = input.image.clone();
                component.local_resources.config = input.claims.clone();
                ensure!(
                    spec.host_interfaces
                        .iter()
                        .all(|interface| interface.config.is_none()),
                    "HTTP template already declares route configuration"
                );
                let mut handlers = spec.host_interfaces.iter_mut().filter(|interface| {
                    interface.namespace == "wasi" && interface.package == "http"
                });
                let handler = handlers
                    .next()
                    .context("HTTP template is missing the wasi:http interface")?;
                ensure!(
                    handlers.next().is_none(),
                    "HTTP template repeats the wasi:http interface"
                );
                ensure!(
                    handler.interfaces == ["handler"],
                    "wasi:http must declare only handler"
                );
                ensure!(
                    handler.config.is_none(),
                    "HTTP template already declares route configuration"
                );
                handler.config = Some(HttpRoute {
                    host: input.route_host.clone(),
                });
            }
        }
    }
    ensure!(
        services == 1 && workloads == 1,
        "HTTP template must contain one Service and one WorkloadDeployment"
    );
    let rendered = documents
        .iter()
        .map(serde_yaml::to_string)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rendered.join("---\n"))
}

/// Render the materializer while retaining its native binding and retry limit.
pub fn render_materializer(template: &str, input: &MaterializerInput) -> anyhow::Result<String> {
    let mut workload: MaterializerDocument =
        serde_yaml::from_str(template).context("parse materializer workload")?;
    workload.metadata.name = input.workload.clone();
    workload.metadata.namespace = input.namespace.clone();
    let spec = &mut workload.spec.template.spec;
    spec.environment = input.namespace.clone();
    let service = &mut spec.service;
    service.image = input.image.clone();
    let config = &mut service.local_resources.config;
    config.tenant = input.tenant.clone();
    config.project = input.event.project.clone();
    config.environment = input.event.environment.clone();
    let environment = &mut service.local_resources.environment.config;
    environment.stream = input.event_stream.clone();
    environment.org = input.event.org.clone();
    environment.project = input.event.project.clone();
    environment.environment = input.event.environment.clone();
    environment.tenant = input.tenant.clone();
    environment.fetch_ms = Some(input.fetch_ms.to_string());
    environment.sweep_ms = Some(input.sweep_ms.to_string());
    serde_yaml::to_string(&workload).context("serialize materializer workload")
}

/// Remove host port mappings from the existing three-node kind template.
pub fn render_kind_cluster(template: &str) -> anyhow::Result<String> {
    let mut cluster: KindCluster = serde_yaml::from_str(template).context("parse kind cluster")?;
    ensure!(
        cluster.nodes.len() == 3,
        "kind template must declare exactly three nodes"
    );
    for node in &mut cluster.nodes {
        node.extra_port_mappings.clear();
    }
    serde_yaml::to_string(&cluster).context("serialize kind cluster")
}

fn one_mut<'a, T>(values: &'a mut [T], path: &str) -> anyhow::Result<&'a mut T> {
    ensure!(values.len() == 1, "{path} must contain exactly one entry");
    Ok(&mut values[0])
}

fn unique_env(values: &[EnvVar]) -> anyhow::Result<()> {
    let mut names = BTreeSet::new();
    for value in values {
        ensure!(
            names.insert(&value.name),
            "host overlay repeats environment variable {}",
            value.name
        );
        ensure!(
            value.value.is_some() != value.value_from.is_some(),
            "{} must have exactly one value or valueFrom",
            value.name
        );
    }
    Ok(())
}

fn env_mut<'a>(values: &'a mut [EnvVar], name: &str) -> anyhow::Result<&'a mut EnvVar> {
    values
        .iter_mut()
        .find(|value| value.name == name)
        .with_context(|| format!("host overlay is missing {name}"))
}

fn replace_arg(args: &mut [String], name: &str, value: &str) -> anyhow::Result<()> {
    let mut selected = args
        .iter_mut()
        .filter(|arg| arg.split_once('=').is_some_and(|(key, _)| key == name));
    let arg = selected
        .next()
        .with_context(|| format!("host overlay is missing {name}"))?;
    ensure!(selected.next().is_none(), "host overlay repeats {name}");
    *arg = format!("{name}={value}");
    Ok(())
}

type Extra = BTreeMap<String, Value>;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HostValues {
    runtime: HostRuntime,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HostRuntime {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image: Option<HostImage>,
    host_groups: Vec<HostGroup>,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HostImage {
    tag: String,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HostGroup {
    namespace: String,
    replicas: u32,
    env: Vec<EnvVar>,
    volumes: Vec<Volume>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    extra_args: Vec<String>,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnvVar {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    value_from: Option<EnvSource>,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnvSource {
    secret_key_ref: SecretKeyRef,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct SecretKeyRef {
    name: String,
    key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    optional: Option<bool>,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Volume {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    secret: Option<SecretVolume>,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SecretVolume {
    secret_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    optional: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    items: Vec<SecretItem>,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct SecretItem {
    key: String,
    path: String,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Metadata {
    name: String,
    namespace: String,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
enum HttpDocument {
    Service(ServiceDocument),
    WorkloadDeployment(HttpDeployment),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServiceDocument {
    api_version: String,
    metadata: Metadata,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HttpDeployment {
    api_version: String,
    metadata: Metadata,
    spec: DeploymentSpec<HttpWorkload>,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MaterializerDocument {
    api_version: String,
    kind: WorkloadKind,
    metadata: Metadata,
    spec: DeploymentSpec<MaterializerWorkload>,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum WorkloadKind {
    WorkloadDeployment,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeploymentSpec<T> {
    replicas: u32,
    template: WorkloadTemplate<T>,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkloadTemplate<T> {
    spec: T,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HttpWorkload {
    environment: String,
    host_interfaces: Vec<HostInterface>,
    components: Vec<HttpComponent>,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MaterializerWorkload {
    environment: String,
    host_interfaces: Vec<HostInterface>,
    service: MaterializerService,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct HostInterface {
    namespace: String,
    package: String,
    version: String,
    interfaces: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    config: Option<HttpRoute>,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HttpRoute {
    host: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HttpComponent {
    image: String,
    local_resources: HttpResources,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HttpResources {
    config: HttpClaims,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MaterializerService {
    image: String,
    local_resources: MaterializerResources,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MaterializerResources {
    config: MaterializerClaims,
    environment: MaterializerEnvironment,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MaterializerClaims {
    #[serde(rename = "wamn.tenant")]
    tenant: String,
    #[serde(rename = "wamn.project")]
    project: String,
    #[serde(rename = "wamn.environment")]
    environment: String,
    #[serde(rename = "wamn.postgres.authority")]
    authority: MaterializerAuthority,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum MaterializerAuthority {
    #[serde(rename = "event-materializer")]
    EventMaterializer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MaterializerEnvironment {
    config: MaterializerConfig,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MaterializerConfig {
    #[serde(rename = "WAMN_MAT_STREAM")]
    stream: String,
    #[serde(rename = "WAMN_MAT_ORG")]
    org: String,
    #[serde(rename = "WAMN_MAT_PROJECT")]
    project: String,
    #[serde(rename = "WAMN_MAT_ENV")]
    environment: String,
    #[serde(rename = "WAMN_MAT_TENANT")]
    tenant: String,
    #[serde(rename = "WAMN_MAT_MAX_DELIVER")]
    max_deliver: String,
    #[serde(
        rename = "WAMN_MAT_FETCH_MS",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    fetch_ms: Option<String>,
    #[serde(
        rename = "WAMN_MAT_SWEEP_MS",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    sweep_ms: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KindCluster {
    kind: ClusterKind,
    api_version: String,
    nodes: Vec<KindNode>,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum ClusterKind {
    Cluster,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KindNode {
    role: NodeRole,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    extra_port_mappings: Vec<PortMapping>,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum NodeRole {
    ControlPlane,
    Worker,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PortMapping {
    container_port: u16,
    host_port: u16,
    #[serde(flatten)]
    extra: Extra,
}

#[cfg(test)]
mod tests;
