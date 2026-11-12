//! Read platform state from Helm, Kubernetes, endpoints, and PostgreSQL without mutation.

use std::collections::BTreeMap;
use std::fmt;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

use anyhow::Context as _;
use base64::Engine as _;
use serde_json::Value;
use wamn_release_control::release_chart::Target;

use super::document::{ClusterDocument, Provider, UpgradeTarget};
use super::{artifact_plan, resources};
use crate::kubectl;

/// A current observation, never a persisted deployment record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Ready,
    Incomplete,
    Failed,
    Unknown,
}

impl fmt::Display for Category {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(match self {
            Self::Ready => "ready",
            Self::Incomplete => "incomplete",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        })
    }
}

/// Safe, source-specific facts from one independent observation.
#[derive(Debug)]
pub struct Observation {
    pub source: String,
    pub category: Category,
    /// A Pod with a deletion timestamp, retained for drain diagnostics.
    pub draining: bool,
    pub facts: Vec<String>,
    pub next: Option<String>,
    /// Images requested by the resource, not proof of running bytes.
    pub images: BTreeMap<String, String>,
    /// Image IDs reported by running pod containers.
    pub running_images: BTreeMap<String, String>,
}

/// The selected artifacts and current owners, kept separate even after failure.
#[derive(Debug)]
pub struct PlatformStatus {
    pub category: Category,
    pub targets: Vec<TargetStatus>,
    pub observations: Vec<Observation>,
    pub warnings: Vec<String>,
}

/// One selected artifact and the independent readiness of its workloads.
#[derive(Debug)]
pub struct TargetStatus {
    pub name: String,
    pub selected: Observation,
    pub warnings: Vec<String>,
    pub category: Category,
    pub observations: Vec<Observation>,
}

/// Read only the affected target, including after an unsuccessful upgrade.
///
/// No service endpoint or ready workload is required for inspection.
pub async fn read_target(
    document: &ClusterDocument,
    target: &Target,
    selection: &UpgradeTarget,
) -> TargetStatus {
    let (name, namespace) = selection.owner(document);
    let values = owner_values(target, namespace, &name).await;
    let default = if !matches!(selection, UpgradeTarget::Host { .. })
        && values
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .is_some_and(|v| v.pointer("/platform/chart").is_none())
    {
        owner_values(target, "platform", "wamn").await
    } else {
        Ok(None)
    };
    target_status(
        document,
        target,
        selection,
        values.as_ref().map(Option::as_ref).map_err(|error| *error),
        default.as_ref().map(Option::as_ref).map_err(|error| *error),
    )
    .await
}

async fn owner_values(target: &Target, namespace: &str, name: &str) -> Result<Option<Value>, ()> {
    let releases = helm_json(target, &["list", "-a", "-n", namespace, "-o", "json"])
        .await
        .map_err(|_| ())?;
    let releases = releases.as_array().ok_or(())?;
    if !releases.iter().any(|release| release["name"] == name) {
        return Ok(None);
    }
    helm_json(
        target,
        &["get", "values", name, "-n", namespace, "-o", "json"],
    )
    .await
    .map(Some)
    .map_err(|_| ())
}

async fn target_status(
    document: &ClusterDocument,
    target: &Target,
    selection: &UpgradeTarget,
    values: Result<Option<&Value>, ()>,
    default: Result<Option<&Value>, ()>,
) -> TargetStatus {
    let (name, namespace) = selection.owner(document);
    let label = match selection {
        UpgradeTarget::Control => "control".into(),
        UpgradeTarget::Identity => "identity".into(),
        UpgradeTarget::Host { release, group } => format!("host:{release}/{group}"),
    };
    let mut selected = match selection {
        UpgradeTarget::Host { group, .. } => {
            let mut selected = observation(
                "selected artifact",
                if matches!(values, Ok(None)) {
                    Category::Incomplete
                } else {
                    Category::Unknown
                },
                if matches!(values, Ok(None)) {
                    "Rerun platform deploy"
                } else {
                    "Restore access to the owning Helm release values"
                },
            );
            if let Some(image) =
                crate::environment::runtime::host_image(values.ok().flatten(), group)
            {
                let registry = image["registry"].as_str().unwrap_or_default();
                if let (Some(repository), Some(tag)) =
                    (image["repository"].as_str(), image["tag"].as_str())
                {
                    let prefix = if registry.is_empty() {
                        String::new()
                    } else {
                        format!("{registry}/")
                    };
                    selected
                        .images
                        .insert("host".into(), format!("{prefix}{repository}:{tag}"));
                    selected.category = Category::Ready;
                    selected.next = None;
                }
            }
            selected
        }
        _ => selected_chart(match values {
            Ok(Some(values)) if values.pointer("/platform/chart").is_none() => default,
            values => values,
        }),
    };
    selected.source = format!("Helm/{namespace}/{name}/selected");
    let (deployment, selector) = match selection {
        UpgradeTarget::Identity => (
            "identity".to_owned(),
            "app.kubernetes.io/name=wamn-identity".to_owned(),
        ),
        UpgradeTarget::Control => (
            format!("hostgroup-{name}"),
            format!("wasmcloud.com/hostgroup={name}"),
        ),
        UpgradeTarget::Host { group, .. } => (
            format!("hostgroup-{group}"),
            format!("wasmcloud.com/hostgroup={group}"),
        ),
    };
    let mut observations = Vec::new();
    let workload = observe_workload(
        target,
        namespace,
        "Deployment",
        &deployment,
        &mut observations,
    )
    .await;
    let mut warnings = Vec::new();
    if selection == &UpgradeTarget::Identity
        && let Some(object) = &workload
    {
        let mail = object
            .pointer("/spec/template/spec/containers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .flat_map(|container| container["env"].as_array().into_iter().flatten())
            .any(|entry| entry["name"] == "RESEND_API_KEY");
        if !mail {
            warnings.push("identity/Deployment/identity: mail is not configured; configure identity mail to deliver invitations".into());
        }
    }
    if selection == &UpgradeTarget::Control {
        observe_workload(
            target,
            namespace,
            "WorkloadDeployment",
            &format!("{name}-http"),
            &mut observations,
        )
        .await;
    } else if let UpgradeTarget::Host { group, .. } = selection {
        let workloads = list(target, namespace, "WorkloadDeployment", &[]).await;
        append_host_workloads(
            &mut observations,
            &format!("{namespace}/WorkloadDeployment"),
            group,
            workloads.ok(),
        );
    }
    let pods = list(target, namespace, "pods", &["-l", &selector]).await;
    let category = append_target_pods(
        &mut observations,
        &format!("{namespace}/pods"),
        workload.as_ref(),
        pods.as_ref().ok(),
    );
    TargetStatus {
        name: label,
        selected,
        warnings,
        category,
        observations,
    }
}

async fn observe_workload(
    target: &Target,
    namespace: &str,
    kind: &str,
    name: &str,
    observations: &mut Vec<Observation>,
) -> Option<Value> {
    let result = get(target, namespace, kind, name).await;
    observations.push(resource(
        &format!("{namespace}/{kind}/{name}"),
        result.as_ref().map(Option::as_ref).map_err(|_| ()),
    ));
    result.ok().flatten()
}

fn observation(source: &str, category: Category, next: &str) -> Observation {
    Observation {
        source: source.to_owned(),
        category,
        draining: false,
        facts: Vec::new(),
        next: (category != Category::Ready).then(|| next.to_owned()),
        images: BTreeMap::new(),
        running_images: BTreeMap::new(),
    }
}

fn combined(categories: impl IntoIterator<Item = Category>) -> Category {
    categories.into_iter().fold(Category::Ready, |left, right| {
        [Category::Failed, Category::Unknown, Category::Incomplete]
            .into_iter()
            .find(|category| *category == left || *category == right)
            .unwrap_or(Category::Ready)
    })
}

/// Read independent owners even when another observation fails.
///
/// Query errors become `unknown` observations. This operation never provisions a resource or credential.
pub async fn read(document: &ClusterDocument, target: &Target) -> anyhow::Result<PlatformStatus> {
    let values = owner_values(target, "platform", "wamn").await;
    let selected = selected_chart(values.as_ref().map(Option::as_ref).map_err(|error| *error));
    let mut targets = Vec::new();
    for selection in [UpgradeTarget::Control, UpgradeTarget::Identity] {
        targets.push(read_target(document, target, &selection).await);
    }
    let mut inherited_hosts = Vec::new();
    let mut inherited_readiness = Vec::new();
    let hosts = helm_json(target, &["list", "-n", "hosts", "-o", "json"]).await;
    for release in hosts
        .as_ref()
        .ok()
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(name) = release["name"].as_str() else {
            continue;
        };
        if name == format!("c-{}", document.org) {
            continue;
        }
        let installed = owner_values(target, "hosts", name).await;
        // An unreadable owner cannot establish whether it has an explicit override.
        if !matches!(installed, Ok(Some(_))) {
            let mut selected = selected_chart(
                installed
                    .as_ref()
                    .map(Option::as_ref)
                    .map_err(|error| *error),
            );
            selected.source = format!("Helm/hosts/{name}/selected");
            if installed.is_err() {
                selected.next = Some("Restore access to the owning Helm release values".into());
            }
            targets.push(TargetStatus {
                name: format!("host:{name}"),
                category: selected.category,
                selected,
                warnings: vec![],
                observations: vec![],
            });
        }
        for group in installed
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .and_then(|v| v.pointer("/runtime-operator/runtime/hostGroups"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(group_name) = group["name"].as_str() else {
                continue;
            };
            let selection = UpgradeTarget::Host {
                release: name.into(),
                group: group_name.into(),
            };
            let observed = target_status(
                document,
                target,
                &selection,
                installed
                    .as_ref()
                    .map(Option::as_ref)
                    .map_err(|error| *error),
                Ok(None),
            )
            .await;
            if group.get("image").is_some() {
                targets.push(observed);
            } else {
                inherited_readiness.push(observed.category);
                inherited_hosts.extend(observed.observations);
            }
        }
    }
    let mut observations = Vec::new();
    if hosts.is_err() {
        observations.push(observation(
            "Helm/hosts",
            Category::Unknown,
            "Restore access to host releases and retry platform status",
        ));
    }
    let mut warnings = Vec::new();
    let control = format!("c-{}", document.org);
    for (namespace, name) in [
        ("platform", "wamn"),
        ("identity", "identity"),
        ("hosts", control.as_str()),
        ("hosts", "wamn-edge"),
    ] {
        let result = helm_json(
            target,
            &[
                "list",
                "-n",
                namespace,
                "--filter",
                &format!("^{name}$"),
                "-o",
                "json",
            ],
        )
        .await;
        observations.push(helm_status(namespace, name, result.as_ref().ok()));
    }
    let control_http = format!("{control}-http");
    let control_certificate = format!("wamn-nats-control-{}", document.org);
    let operator_certificate = format!("operator-{}", document.org);
    for (namespace, kind, name) in [
        ("cert-manager", "Deployment", "cert-manager"),
        ("cert-manager", "Deployment", "cert-manager-cainjector"),
        ("cert-manager", "Deployment", "cert-manager-webhook"),
        ("cnpg-system", "Deployment", "cnpg-controller-manager"),
        ("platform", "Deployment", "runtime-operator"),
        ("platform", "StatefulSet", "wamn-nats"),
        ("platform", "Cluster.postgresql.cnpg.io", "wamn-pg"),
        ("hosts", "Deployment", "wamn-edge"),
        ("hosts", "Certificate", "wamn-edge-tls"),
        ("cert-manager", "Certificate", "wasmcloud-ca"),
        ("platform", "Certificate", "wasmcloud-nats-tls"),
        ("platform", "Certificate", "wasmcloud-operator-tls"),
        ("platform", "Certificate", "wamn-ctl-nats"),
        ("platform", "Certificate", "wamn-provisioning-nats"),
        ("hosts", "Certificate", "wasmcloud-runtime-tls"),
        ("hosts", "Certificate", "wasmcloud-data-tls"),
        ("hosts", "Certificate", control_certificate.as_str()),
        ("identity", "Certificate", "identity"),
        ("identity", "Certificate", "wamn-identity-nats"),
        ("identity", "Certificate", "identity-operator-ca"),
        ("identity", "Certificate", operator_certificate.as_str()),
        ("identity", "Issuer", "identity-operator-selfsigned"),
        ("identity", "Issuer", "identity-operator"),
        ("platform", "ClusterIssuer", "wamn-selfsigned"),
        ("platform", "ClusterIssuer", "wasmcloud-ca"),
    ] {
        let result = get(target, namespace, kind, name).await;
        let source = format!("{namespace}/{kind}/{name}");
        observations.push(resource(
            &source,
            result.as_ref().map(Option::as_ref).map_err(|_| ()),
        ));
    }
    for (namespace, selector) in [
        ("platform", String::new()),
        ("cert-manager", String::new()),
        ("cnpg-system", String::new()),
        ("hosts", "app.kubernetes.io/name=wamn-edge".into()),
    ] {
        let arguments = if selector.is_empty() {
            vec![]
        } else {
            vec!["-l", selector.as_str()]
        };
        let result = list(target, namespace, "pods", &arguments).await;
        append_resources(
            &mut observations,
            &format!("{namespace}/pods"),
            result.as_ref().ok(),
        );
    }
    for (namespace, service) in [
        ("platform", "wamn-pg-rw"),
        ("identity", "identity"),
        ("hosts", control_http.as_str()),
        ("hosts", "wamn-edge"),
    ] {
        let result = list(
            target,
            namespace,
            "endpointslices.discovery.k8s.io",
            &["-l", &format!("kubernetes.io/service-name={service}")],
        )
        .await;
        observations.push(endpoints(
            &format!("{namespace}/Service/{service}"),
            result.as_ref().ok(),
        ));
    }
    let database = tokio::time::timeout(Duration::from_secs(40), database(target)).await;
    observations.push(database.ok().and_then(Result::ok).unwrap_or_else(|| observation(
        "PostgreSQL/wamn_system", Category::Unknown,
        "Read platform/wamn-pg-rw and wamn-pg-superuser; restore database access and retry status",
    )));
    for identity in [true, false] {
        let result =
            tokio::time::timeout(Duration::from_secs(40), probe(target, document, identity)).await;
        observations.push(result.ok().and_then(Result::ok).unwrap_or_else(|| observation(
            if identity { "identity/HTTPS/healthz" } else { "control/HTTP/control/mine" },
            Category::Unknown, "Inspect this service's endpoints, serving certificate, and process logs; retry status",
        )));
    }
    if document.provider == Provider::Kind {
        let result = async {
            reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()?
                .get(format!(
                    "http://{}/v2/",
                    document.registry.split('/').next().unwrap_or_default()
                ))
                .send()
                .await
        }
        .await;
        observations.push(observation("local registry", match result {
            Ok(response) if response.status().is_success() => Category::Ready,
            Ok(_) => Category::Failed,
            Err(_) => Category::Unknown,
        }, "Inspect the local registry container and published port; rerun platform deploy if it is missing"));
    }
    if document.buckets.web.is_empty() {
        warnings.push("cluster.buckets.web: platform UI storage is not selected; select storage and upload UI when needed".into());
    }
    if document.buckets.otel.is_empty() {
        warnings.push("cluster.buckets.otel: telemetry storage is not selected; configure telemetry when needed".into());
    }
    let (mut default_observations, other): (Vec<_>, Vec<_>) =
        observations.into_iter().partition(|item| {
            item.source == "platform/Deployment/runtime-operator"
                || item.source == "Helm/platform/wamn"
        });
    observations = other;
    let pods = list(
        target,
        "platform",
        "pods",
        &["-l", "wasmcloud.com/name=runtime-operator"],
    )
    .await;
    append_resources(
        &mut default_observations,
        "platform/pods",
        pods.as_ref().ok(),
    );
    let default_category = combined(
        default_observations
            .iter()
            .filter(|o| o.source != "Helm/platform/wamn")
            .map(|o| o.category)
            .chain(inherited_readiness),
    );
    default_observations.extend(inherited_hosts);
    targets.insert(
        0,
        TargetStatus {
            name: "platform default".into(),
            selected,
            warnings: vec![],
            category: default_category,
            observations: default_observations,
        },
    );
    let category = combined(
        targets
            .iter()
            .flat_map(|item| [item.selected.category, item.category])
            .chain(observations.iter().map(|item| item.category)),
    );
    Ok(PlatformStatus {
        category,
        targets,
        observations,
        warnings,
    })
}

async fn helm_json(target: &Target, arguments: &[&str]) -> anyhow::Result<Value> {
    let arguments: Vec<_> = arguments.iter().map(std::ffi::OsStr::new).collect();
    let bytes = tokio::time::timeout(Duration::from_secs(15), resources::helm(target, &arguments))
        .await??;
    Ok(serde_json::from_slice(&bytes)?)
}

async fn get(
    target: &Target,
    namespace: &str,
    kind: &str,
    name: &str,
) -> anyhow::Result<Option<Value>> {
    let result = tokio::time::timeout(
        Duration::from_secs(15),
        kubectl::get(&target.kubeconfig, &target.context, namespace, kind, name),
    )
    .await?;
    match result {
        // An API the server does not serve yet holds no object: its CRD is not installed.
        Err(error) if format!("{error:#}").contains("doesn't have a resource type") => Ok(None),
        result => result,
    }
}

async fn list(
    target: &Target,
    namespace: &str,
    kind: &str,
    extra: &[&str],
) -> anyhow::Result<Value> {
    let mut arguments = vec!["get", kind, "-o", "json"];
    arguments.extend_from_slice(extra);
    let body = tokio::time::timeout(
        Duration::from_secs(15),
        kubectl::kubectl(
            &target.kubeconfig,
            &target.context,
            namespace,
            &arguments,
            None,
        ),
    )
    .await??;
    Ok(serde_json::from_str(&body)?)
}

fn selected_chart(values: Result<Option<&Value>, ()>) -> Observation {
    let mut result = observation(
        "Helm/platform/wamn/platform.chart",
        Category::Unknown,
        "Restore access to the exact chart path recorded in Helm, or deploy an explicitly selected prepared chart",
    );
    if matches!(values, Ok(None)) {
        result.category = Category::Incomplete;
        result.next = Some("Rerun platform deploy".into());
        return result;
    }
    let Some(path) = values
        .ok()
        .flatten()
        .and_then(|value| value.pointer("/platform/chart"))
        .and_then(Value::as_str)
    else {
        return result;
    };
    result.facts.push(format!("chart={path}"));
    let Ok(chart) = artifact_plan::select(Path::new(path)) else {
        return result;
    };
    result.category = Category::Ready;
    result.next = None;
    result.facts.push(format!("version={}", chart.version));
    if let Ok(host) = chart.image("host") {
        result.images.insert("host".into(), host.into());
    }
    image_references(&chart.values()["platform"], "platform", &mut result.images);
    result
}

fn helm_status(namespace: &str, name: &str, response: Option<&Value>) -> Observation {
    let releases = response.and_then(Value::as_array);
    let release = releases.and_then(|entries| entries.iter().find(|entry| entry["name"] == name));
    let state = release.and_then(|entry| entry["status"].as_str());
    let category = match state {
        Some("deployed") => Category::Ready,
        Some("failed") => Category::Failed,
        Some(
            "pending-install" | "pending-upgrade" | "pending-rollback" | "uninstalling"
            | "uninstalled" | "superseded",
        ) => Category::Incomplete,
        None if releases.is_some() && release.is_none() => Category::Incomplete,
        _ => Category::Unknown,
    };
    let mut result = observation(
        &format!("Helm/{namespace}/{name}"),
        category,
        &format!(
            "Read helm status {name} -n {namespace} and repair the reported release failure; rerun platform deploy"
        ),
    );
    if let Some(state) = state {
        result.facts.push(format!("status={state}"));
    }
    result
}

fn image_references(value: &Value, path: &str, images: &mut BTreeMap<String, String>) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                if key == "image" {
                    if let Some(image) = value.as_str().filter(|image| !image.is_empty()) {
                        images.insert(path.into(), image.into());
                    }
                } else {
                    image_references(value, &format!("{path}/{key}"), images);
                }
            }
        }
        Value::Array(entries) => {
            for (index, entry) in entries.iter().enumerate() {
                image_references(entry, &format!("{path}/{index}"), images);
            }
        }
        _ => {}
    }
}

fn resource(source: &str, response: Result<Option<&Value>, ()>) -> Observation {
    let category = match response {
        Ok(Some(object)) => classify(object),
        Ok(None) => Category::Incomplete,
        Err(()) => Category::Unknown,
    };
    let mut result = observation(
        source,
        category,
        &format!(
            "Inspect {source} conditions and events; repair the owning resource and rerun platform deploy"
        ),
    );
    if let Ok(Some(object)) = response {
        result.draining =
            object["kind"] == "Pod" && !object["metadata"]["deletionTimestamp"].is_null();
        image_references(&object["spec"], "spec", &mut result.images);
        for field in ["generation", "observedGeneration"] {
            let value = if field == "generation" {
                &object["metadata"][field]
            } else {
                &object["status"][field]
            };
            if let Some(value) = value.as_u64() {
                result.facts.push(format!("{field}={value}"));
            }
        }
        for condition in object["status"]["conditions"]
            .as_array()
            .into_iter()
            .flatten()
        {
            // Messages can contain credentials from controllers. Keep only condition identifiers.
            result.facts.push(format!(
                "{}={} reason={} observedGeneration={}",
                condition["type"].as_str().unwrap_or("unknown"),
                condition["status"].as_str().unwrap_or("Unknown"),
                condition["reason"].as_str().unwrap_or("unknown"),
                condition["observedGeneration"]
            ));
        }
        for field in ["phase", "currentPrimary"] {
            if let Some(value) = object["status"][field].as_str() {
                result.facts.push(format!("{field}={value}"));
            }
        }
        for field in ["containerStatuses", "initContainerStatuses"] {
            for container in object["status"][field].as_array().into_iter().flatten() {
                if container.pointer("/state/running").is_some()
                    && let (Some(name), Some(id)) =
                        (container["name"].as_str(), container["imageID"].as_str())
                {
                    result.running_images.insert(name.into(), id.into());
                }
                if let Some(reason) = container
                    .pointer("/state/waiting/reason")
                    .and_then(Value::as_str)
                {
                    result
                        .facts
                        .push(format!("container={} reason={reason}", container["name"]));
                    if matches!(
                        reason,
                        "ErrImagePull" | "ImagePullBackOff" | "InvalidImageName"
                    ) {
                        result.next = Some(format!(
                            "Inspect {source} image references and registry access; publish the missing image or deploy a corrected chart"
                        ));
                    }
                }
            }
        }
    }
    result
}

fn classify(object: &Value) -> Category {
    let status = &object["status"];
    let kind = object["kind"].as_str().unwrap_or_default();
    if !object["metadata"]["deletionTimestamp"].is_null() {
        return Category::Incomplete;
    }
    let generation = object["metadata"]["generation"].as_u64();
    let observed = status["observedGeneration"].as_u64();
    if generation
        .zip(observed)
        .is_some_and(|(current, observed)| observed < current)
    {
        return Category::Incomplete;
    }
    let conditions: Vec<_> = status["conditions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|condition| {
            generation
                .zip(condition["observedGeneration"].as_u64().or(observed))
                .is_none_or(|(current, observed)| observed >= current)
        })
        .collect();
    if conditions.iter().any(|condition| {
        (matches!(
            condition["type"].as_str(),
            Some("Failed" | "Stalled" | "Denied")
        ) && condition["status"] == "True")
            || (condition["type"] == "Progressing"
                && condition["status"] == "False"
                && condition["reason"] == "ProgressDeadlineExceeded")
            || (condition["status"] == "False"
                && matches!(
                    condition["reason"].as_str(),
                    Some("Failed" | "InvalidSpec" | "InvalidRequest")
                ))
    }) {
        return Category::Failed;
    }
    if kind == "Pod" {
        if status["phase"] == "Failed" {
            return Category::Failed;
        }
        if status["phase"] == "Unknown" {
            return Category::Unknown;
        }
        return if status["phase"] == "Succeeded"
            || (status["phase"] == "Running"
                && conditions
                    .iter()
                    .any(|condition| condition["type"] == "Ready" && condition["status"] == "True"))
        {
            Category::Ready
        } else {
            Category::Incomplete
        };
    }
    if conditions
        .iter()
        .any(|condition| condition["status"] == "Unknown")
    {
        return Category::Unknown;
    }
    match kind {
        "Deployment" | "StatefulSet" => {
            let desired = object["spec"]["replicas"].as_u64().unwrap_or(1);
            let current = observed
                .zip(generation)
                .is_some_and(|(observed, generation)| observed >= generation);
            let ready = desired > 0
                && status["readyReplicas"].as_u64().unwrap_or(0) >= desired
                && status["updatedReplicas"].as_u64().unwrap_or(0) >= desired;
            if current
                && ready
                && (kind == "StatefulSet"
                    || status["availableReplicas"].as_u64().unwrap_or(0) >= desired)
            {
                Category::Ready
            } else {
                Category::Incomplete
            }
        }
        "WorkloadDeployment" => {
            let desired = object["spec"]["replicas"].as_u64().unwrap_or(1);
            let ready = status["replicas"]["ready"].as_u64().unwrap_or(0) >= desired && desired > 0;
            if ready
                && conditions.iter().any(|condition| {
                    matches!(condition["type"].as_str(), Some("Ready" | "Available"))
                        && condition["status"] == "True"
                })
            {
                Category::Ready
            } else {
                Category::Incomplete
            }
        }
        "Certificate" | "Issuer" | "ClusterIssuer" | "Cluster" => {
            let ready = conditions
                .iter()
                .any(|condition| condition["type"] == "Ready" && condition["status"] == "True");
            if ready
                && (kind != "Cluster"
                    || (status["currentPrimary"]
                        .as_str()
                        .is_some_and(|name| !name.is_empty())
                        && status["readyInstances"].as_u64().unwrap_or(0) > 0))
            {
                Category::Ready
            } else {
                Category::Incomplete
            }
        }
        _ => Category::Unknown,
    }
}

fn append_target_pods(
    observations: &mut Vec<Observation>,
    source: &str,
    deployment: Option<&Value>,
    pods: Option<&Value>,
) -> Category {
    let replacements_ready = deployment.is_some_and(|deployment| {
        classify(deployment) == Category::Ready
            && pods
                .and_then(|pods| pods["items"].as_array())
                .is_some_and(|pods| {
                    pods.iter()
                        .filter(|pod| {
                            pod["kind"] == "Pod"
                                && pod["metadata"]["deletionTimestamp"].is_null()
                                && pod["status"]["phase"] == "Running"
                                && classify(pod) == Category::Ready
                        })
                        .count() as u64
                        >= deployment["spec"]["replicas"].as_u64().unwrap_or(1)
                })
    }) && observations.iter().all(|o| o.category == Category::Ready);
    append_resources(observations, source, pods);
    combined(
        observations
            .iter()
            .filter(|o| !replacements_ready || !o.draining)
            .map(|o| o.category),
    )
}

fn append_resources(observations: &mut Vec<Observation>, source: &str, response: Option<&Value>) {
    match response.and_then(|response| response["items"].as_array()) {
        Some(items) if !items.is_empty() => {
            for object in items {
                // Completed Jobs describe past attempts, not current service readiness.
                if object["kind"] == "Pod"
                    && object["metadata"]["ownerReferences"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|owner| owner["kind"] == "Job")
                {
                    continue;
                }
                let name = object["metadata"]["name"].as_str().unwrap_or("unknown");
                observations.push(resource(&format!("{source}/{name}"), Ok(Some(object))));
            }
        }
        Some(_) => observations.push(resource(source, Ok(None))),
        None => observations.push(resource(source, Err(()))),
    }
}

fn append_host_workloads(
    observations: &mut Vec<Observation>,
    source: &str,
    group: &str,
    mut response: Option<Value>,
) {
    if let Some(items) = response
        .as_mut()
        .and_then(|response| response["items"].as_array_mut())
    {
        // The application renderer and materializer writer use this placement selector.
        items.retain(|object| {
            object
                .pointer("/spec/template/spec/hostSelector/hostgroup")
                .and_then(Value::as_str)
                == Some(group)
        });
        if items.is_empty() {
            return;
        }
    }
    append_resources(observations, source, response.as_ref());
}

fn endpoints(source: &str, response: Option<&Value>) -> Observation {
    let items = response.and_then(|value| value["items"].as_array());
    let mut result = observation(
        source,
        if items.is_some() {
            Category::Incomplete
        } else {
            Category::Unknown
        },
        &format!("Inspect {source} selectors and endpoint readiness; restore the backing workload"),
    );
    for slice in items.into_iter().flatten() {
        for endpoint in slice["endpoints"].as_array().into_iter().flatten() {
            if endpoint["conditions"]["ready"] == true
                && endpoint["conditions"]["terminating"] != true
            {
                for address in endpoint["addresses"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    result
                        .facts
                        .push(format!("address={address} ports={}", slice["ports"]));
                    result.category = Category::Ready;
                    result.next = None;
                }
            }
        }
    }
    result
}

fn secret_bytes(secret: &Value, key: &str) -> anyhow::Result<Vec<u8>> {
    let value = secret["data"][key]
        .as_str()
        .context("required Secret key is absent")?;
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .context("invalid Secret encoding")
}

async fn database(target: &Target) -> anyhow::Result<Observation> {
    let Some(credentials) = get(target, "platform", "secret", "wamn-pg-superuser").await? else {
        return Ok(observation(
            "platform/Secret/wamn-pg-superuser",
            Category::Incomplete,
            "Inspect the CNPG cluster and restore its superuser Secret before reading system initialization",
        ));
    };
    let forward = resources::port_forward(target, "platform", "service/wamn-pg-rw", 5432).await?;
    let mut configuration = tokio_postgres::Config::new();
    configuration
        .host("127.0.0.1")
        .port(forward.port)
        .dbname("wamn_system")
        .user(std::str::from_utf8(&secret_bytes(
            &credentials,
            "username",
        )?)?)
        .password(secret_bytes(&credentials, "password")?)
        .connect_timeout(Duration::from_secs(5))
        .options("-c default_transaction_read_only=on -c statement_timeout=5000");
    let (mut client, connection) = configuration.connect(tokio_postgres::NoTls).await?;
    // The connection future stays scoped to this observation, including cancellation.
    let read = async {
        let transaction = client
            .build_transaction()
            .read_only(true)
            .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
            .start()
            .await?;
        let state = crate::provision_system::system_state(&transaction).await?;
        let report = wamn_package_control::schema_baseline::observe_system(&transaction).await?;
        let result = database_state(&state, &report);
        transaction.rollback().await?;
        Ok(result)
    };
    tokio::pin!(read);
    tokio::pin!(connection);
    tokio::select! {
        result = &mut read => result,
        _ = &mut connection => anyhow::bail!("database observation connection closed"),
    }
}

fn database_state(
    state: &crate::provision_system::SystemState,
    report: &wamn_package_control::schema_baseline::SchemaReport,
) -> Observation {
    use crate::provision_system::SystemState;
    // system_state compares with its executable. Status reports only recorded database facts.
    let category = match state {
        SystemState::Complete | SystemState::BaselineMismatch { .. }
            if report.initialization.is_some() =>
        {
            Category::Ready
        }
        SystemState::Absent | SystemState::Incomplete => Category::Incomplete,
        _ => Category::Unknown,
    };
    let mut result = observation(
        "PostgreSQL/wamn_system",
        category,
        match state {
            SystemState::Absent => {
                "Run platform deploy with the selected chart to initialize the absent system schema"
            }
            _ => {
                "Inspect the system baseline and restore a verified database backup; do not reset or replay partial initialization"
            }
        },
    );
    result
        .facts
        .extend(report.to_string().lines().map(str::to_owned));
    result
}

async fn probe(
    target: &Target,
    document: &ClusterDocument,
    identity: bool,
) -> anyhow::Result<Observation> {
    let (namespace, service, port) = if identity {
        ("identity", "identity".to_owned(), 443)
    } else {
        ("hosts", format!("c-{}-http", document.org), 80)
    };
    if get(target, namespace, "service", &service).await?.is_none() {
        return Ok(observation(
            &format!("{namespace}/Service/{service}"),
            Category::Incomplete,
            "Rerun platform deploy to create this service",
        ));
    }
    let forward =
        resources::port_forward(target, namespace, &format!("service/{service}"), port).await?;
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none());
    let url = if identity {
        let Some(tls) = get(target, "identity", "secret", "identity-tls").await? else {
            return Ok(observation(
                "identity/Secret/identity-tls",
                Category::Incomplete,
                "Inspect identity/Certificate/identity and restore its serving Secret",
            ));
        };
        builder = builder
            .add_root_certificate(reqwest::Certificate::from_pem(&secret_bytes(
                &tls, "ca.crt",
            )?)?)
            .resolve(
                "identity.identity.svc.cluster.local",
                ([127, 0, 0, 1], forward.port).into(),
            );
        format!(
            "https://identity.identity.svc.cluster.local:{}/healthz",
            forward.port
        )
    } else {
        format!(
            "http://127.0.0.1:{}/wamn_control/control/mine",
            forward.port
        )
    };
    let response = builder
        .build()?
        .get(&url)
        .header(
            reqwest::header::HOST,
            if identity {
                "identity.identity.svc.cluster.local"
            } else {
                &document.route_hosts.control
            },
        )
        .send()
        .await?;
    let expected = if identity {
        reqwest::StatusCode::OK
    } else {
        reqwest::StatusCode::UNAUTHORIZED
    };
    let mut result = observation(
        if identity {
            "identity/HTTPS/healthz"
        } else {
            "control/HTTP/control/mine"
        },
        if response.status() == expected {
            Category::Ready
        } else {
            Category::Failed
        },
        "Inspect the service logs and its database and authorization dependencies",
    );
    result.facts.push(format!(
        "HTTP {} via {namespace}/Service/{service}",
        response.status().as_u16()
    ));
    Ok(result)
}

/// Write safe facts and next actions without exposing Secrets or command diagnostics.
pub fn write(status: &PlatformStatus, output: &mut impl Write) -> anyhow::Result<()> {
    writeln!(output, "platform: {}", status.category)?;
    for target in &status.targets {
        write_target(target, output)?;
    }
    for item in &status.observations {
        write_observation(item, false, output)?;
    }
    for warning in &status.warnings {
        writeln!(output, "warning: {warning}")?;
    }
    Ok(())
}

/// Write one target without claiming readiness of the whole platform.
pub fn write_target(target: &TargetStatus, output: &mut impl Write) -> anyhow::Result<()> {
    writeln!(output, "target {}: {}", target.name, target.category)?;
    write_observation(&target.selected, true, output)?;
    for item in &target.observations {
        write_observation(item, false, output)?;
    }
    for warning in &target.warnings {
        writeln!(output, "warning: {warning}")?;
    }
    Ok(())
}

fn write_observation(
    item: &Observation,
    selected: bool,
    output: &mut impl Write,
) -> anyhow::Result<()> {
    writeln!(output, "{}: {}", item.source, item.category)?;
    if item.draining {
        writeln!(output, "  draining")?;
    }
    for fact in &item.facts {
        writeln!(output, "  {fact}")?;
    }
    for (name, image) in &item.images {
        writeln!(
            output,
            "  {} {name}: {image}",
            if selected {
                "selected image"
            } else {
                "resource image"
            }
        )?;
    }
    for (name, image) in &item.running_images {
        writeln!(output, "  running image ID {name}: {image}")?;
    }
    if let Some(next) = &item.next {
        writeln!(output, "  next: {next}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn namespace_without_releases_is_an_incomplete_platform() {
        let namespace = json!({"kind":"Namespace","metadata":{"name":"platform"}});
        let releases = json!([]);
        let pods = json!({"items":[]});
        let mut observations = vec![
            helm_status(
                namespace["metadata"]["name"].as_str().unwrap(),
                "wamn",
                Some(&releases),
            ),
            selected_chart(Ok(None)),
            resource("platform/Deployment/runtime-operator", Ok(None)),
        ];
        for selection in [UpgradeTarget::Control, UpgradeTarget::Identity] {
            let mut selected = selected_chart(Ok(None));
            selected.source = format!("{selection:?}/selected");
            observations.push(selected);
        }
        append_resources(&mut observations, "platform/pods", Some(&pods));
        assert_eq!(
            combined(observations.iter().map(|item| item.category)),
            Category::Incomplete
        );
    }

    #[test]
    fn helm_distinguishes_missing_failed_pending_and_unreadable_releases() {
        for (state, expected) in [
            ("deployed", Category::Ready),
            ("failed", Category::Failed),
            ("pending-upgrade", Category::Incomplete),
            ("unknown", Category::Unknown),
        ] {
            let response = json!([{"name":"wamn","namespace":"platform","revision":"2","status":state,"chart":"runtime-operator-2.11.0"}]);
            assert_eq!(
                helm_status("platform", "wamn", Some(&response)).category,
                expected
            );
        }
        assert_eq!(
            helm_status("platform", "wamn", Some(&json!([]))).category,
            Category::Incomplete
        );
        assert_eq!(
            helm_status("platform", "wamn", None).category,
            Category::Unknown
        );
    }

    #[test]
    fn deployment_requires_the_current_generation_and_complete_rollout() {
        let mut deployment = json!({
            "kind":"Deployment", "metadata":{"generation":4}, "spec":{"replicas":2},
            "status":{"observedGeneration":4,"readyReplicas":2,"updatedReplicas":2,"availableReplicas":2,
                "conditions":[{"type":"Available","status":"True"}]}
        });
        assert_eq!(classify(&deployment), Category::Ready);
        deployment["status"]["observedGeneration"] = json!(3);
        assert_eq!(classify(&deployment), Category::Incomplete);
        deployment["status"]["observedGeneration"] = json!(4);
        deployment["status"]["updatedReplicas"] = json!(1);
        assert_eq!(classify(&deployment), Category::Incomplete);
        deployment["status"]["conditions"] =
            json!([{"type":"Progressing","status":"False","reason":"ProgressDeadlineExceeded"}]);
        assert_eq!(classify(&deployment), Category::Failed);
    }

    #[test]
    fn certificate_and_workload_conditions_do_not_reuse_stale_success_or_failure() {
        for kind in ["Certificate", "WorkloadDeployment"] {
            let mut object = json!({
                "kind":kind,"metadata":{"generation":2},"spec":{"replicas":1},
                "status":{"replicas":{"ready":1,"current":1,"expected":1},
                    "conditions":[{"type":"Ready","status":"True","observedGeneration":2}]}
            });
            assert_eq!(classify(&object), Category::Ready);
            object["status"]["conditions"][0]["observedGeneration"] = json!(1);
            assert_eq!(classify(&object), Category::Incomplete);
            object["status"]["conditions"][0] =
                json!({"type":"Ready","status":"False","reason":"Failed","observedGeneration":1});
            assert_eq!(classify(&object), Category::Incomplete);
            object["status"]["conditions"][0]["observedGeneration"] = json!(2);
            assert_eq!(classify(&object), Category::Failed);
            object["status"]["conditions"][0] =
                json!({"type":"Ready","status":"Unknown","observedGeneration":2});
            assert_eq!(classify(&object), Category::Unknown);
        }
    }

    #[test]
    fn cnpg_requires_a_ready_primary() {
        let mut cluster = json!({"kind":"Cluster","status":{
            "conditions":[{"type":"Ready","status":"True"}],
            "currentPrimary":"wamn-pg-1","readyInstances":1,"phase":"Cluster in healthy state"
        }});
        assert_eq!(classify(&cluster), Category::Ready);
        cluster["status"]["currentPrimary"] = Value::Null;
        assert_eq!(classify(&cluster), Category::Incomplete);
    }

    #[test]
    fn resource_queries_distinguish_absence_from_failure_and_keep_independent_results() {
        let source = "platform/Deployment/runtime-operator";
        assert_eq!(resource(source, Ok(None)).category, Category::Incomplete);
        assert_eq!(resource(source, Err(())).category, Category::Unknown);
        let mut observations = vec![resource(source, Err(()))];
        append_resources(
            &mut observations,
            "identity/certificates",
            Some(&json!({"items":[{
                "kind":"Certificate","metadata":{"name":"identity"},
                "status":{"conditions":[{"type":"Ready","status":"True"}]}
            }]})),
        );
        assert_eq!(observations.last().unwrap().category, Category::Ready);
        assert_eq!(
            combined(observations.iter().map(|item| item.category)),
            Category::Unknown
        );
        assert_eq!(
            combined([Category::Ready, Category::Failed, Category::Unknown]),
            Category::Failed
        );
    }

    #[test]
    fn host_readiness_includes_only_assigned_workloads() {
        let host = json!({
            "kind":"Deployment", "metadata":{"generation":1}, "spec":{"replicas":1},
            "status":{"observedGeneration":1,"readyReplicas":1,"updatedReplicas":1,"availableReplicas":1}
        });
        for (state, expected) in [
            ("ready", Category::Ready),
            ("failed", Category::Failed),
            ("incomplete", Category::Incomplete),
            ("stale-condition", Category::Incomplete),
            ("stale-status", Category::Incomplete),
            ("unreadable", Category::Unknown),
            ("other-group", Category::Ready),
            ("empty", Category::Ready),
        ] {
            let mut workload = json!({
                "kind":"WorkloadDeployment", "metadata":{"name":"alpha-http","generation":2},
                "spec":{"replicas":1,"template":{"spec":{"hostSelector":{"hostgroup":"alpha"}}}},
                "status":{"replicas":{"ready":1},
                    "conditions":[{"type":"Ready","status":"True","observedGeneration":2}]}
            });
            match state {
                "failed" | "other-group" => {
                    workload["status"]["conditions"][0] = json!({"type":"Ready","status":"False","reason":"Failed","observedGeneration":2});
                    if state == "other-group" {
                        workload["spec"]["template"]["spec"]["hostSelector"]["hostgroup"] =
                            "beta".into();
                    }
                }
                "incomplete" => workload["status"]["replicas"]["ready"] = json!(0),
                "stale-condition" => {
                    workload["status"]["conditions"][0]["observedGeneration"] = json!(1);
                }
                "stale-status" => workload["status"]["observedGeneration"] = json!(1),
                _ => {}
            }
            let response = match state {
                "unreadable" => None,
                "empty" => Some(json!({"items":[]})),
                _ => Some(json!({"items":[workload]})),
            };
            let mut observations = vec![resource(
                "hosts/Deployment/hostgroup-alpha",
                Ok(Some(&host)),
            )];
            append_host_workloads(
                &mut observations,
                "hosts/WorkloadDeployment",
                "alpha",
                response,
            );
            let pods = json!({"items":[
                {"kind":"Pod","metadata":{"name":"old","deletionTimestamp":"2026-11-09T18:05:00Z"}},
                {"kind":"Pod","metadata":{"name":"replacement"},
                    "status":{"phase":"Running","conditions":[{"type":"Ready","status":"True"}]}}
            ]});
            assert_eq!(
                append_target_pods(&mut observations, "hosts/pods", Some(&host), Some(&pods)),
                expected,
                "{state}"
            );
            assert_eq!(
                observations.iter().find(|o| o.draining).unwrap().category,
                Category::Incomplete
            );
        }
    }

    #[test]
    fn target_readiness_requires_ready_replacements_before_ignoring_draining_pods() {
        for state in [
            "ready",
            "missing",
            "unready",
            "too-few",
            "succeeded",
            "stale",
            "unobserved",
            "not-updated",
            "not-ready",
            "not-available",
        ] {
            let mut deployment = json!({
                "kind":"Deployment", "metadata":{"generation":2}, "spec":{"replicas":1},
                "status":{"observedGeneration":2,"updatedReplicas":1,"readyReplicas":1,"availableReplicas":1}
            });
            let mut pods = json!({"items":[
                {"kind":"Pod","metadata":{"name":"old","deletionTimestamp":"2026-11-09T18:05:00Z"},
                    "spec":{"containers":[{"name":"host","image":"registry/host:old"}]},
                    "status":{"containerStatuses":[{"name":"host","imageID":"sha256:old","state":{"running":{}}}]}},
                {"kind":"Pod","metadata":{"name":"replacement","deletionTimestamp":null},
                    "status":{"phase":"Running","conditions":[{"type":"Ready","status":"True"}]}}
            ]});
            match state {
                "missing" => {
                    pods["items"].as_array_mut().unwrap().pop();
                }
                "unready" => pods["items"][1]["status"]["conditions"][0]["status"] = json!("False"),
                "succeeded" => pods["items"][1]["status"]["phase"] = json!("Succeeded"),
                "too-few" => {
                    deployment["spec"]["replicas"] = json!(2);
                    for field in ["updatedReplicas", "readyReplicas", "availableReplicas"] {
                        deployment["status"][field] = json!(2);
                    }
                }
                "stale" => deployment["status"]["observedGeneration"] = json!(1),
                "unobserved" => deployment["status"]["observedGeneration"] = Value::Null,
                "not-updated" => deployment["status"]["updatedReplicas"] = json!(0),
                "not-ready" => deployment["status"]["readyReplicas"] = json!(0),
                "not-available" => deployment["status"]["availableReplicas"] = json!(0),
                _ => {}
            }
            let mut observations = vec![resource("hosts/Deployment/host", Ok(Some(&deployment)))];
            let category = append_target_pods(
                &mut observations,
                "hosts/pods",
                Some(&deployment),
                Some(&pods),
            );
            assert_eq!(
                category,
                if state == "ready" {
                    Category::Ready
                } else {
                    Category::Incomplete
                },
                "{state}"
            );
            let draining = observations.iter().find(|o| o.draining).unwrap();
            assert_eq!(draining.category, Category::Incomplete);
            assert_eq!(
                draining.images["spec/containers/0"],
                pods["items"][0]["spec"]["containers"][0]["image"]
            );
            assert_eq!(
                draining.running_images["host"],
                pods["items"][0]["status"]["containerStatuses"][0]["imageID"]
            );
            if state == "ready" {
                pods["items"].as_array_mut().unwrap().remove(0);
                let mut later = vec![resource("hosts/Deployment/host", Ok(Some(&deployment)))];
                assert_eq!(
                    append_target_pods(&mut later, "hosts/pods", Some(&deployment), Some(&pods)),
                    Category::Ready
                );
                assert!(later.iter().all(|o| !o.draining));
            }
        }
    }

    #[test]
    fn resource_images_and_running_container_ids_remain_distinct() {
        let object = json!({
            "kind":"Pod", "spec":{"containers":[{"name":"host","image":"registry/host:next@sha256:abc"}]},
            "status":{"phase":"Running","conditions":[{"type":"Ready","status":"True"}],
                "containerStatuses":[{"name":"host","image":"registry/host:old","imageID":"containerd://sha256:def","state":{"running":{"startedAt":"2026-11-06T00:00:00Z"}}}]}
        });
        let status = resource("hosts/pods/host", Ok(Some(&object)));
        assert_eq!(status.category, Category::Ready);
        assert_eq!(
            status.images["spec/containers/0"],
            object["spec"]["containers"][0]["image"]
        );
        assert_eq!(
            status.running_images["host"],
            object["status"]["containerStatuses"][0]["imageID"]
        );
        let mut failed = object;
        failed["status"]["phase"] = json!("Failed");
        assert_eq!(classify(&failed), Category::Failed);
    }

    #[test]
    fn selected_images_are_enumerated_and_an_inaccessible_chart_stays_unknown() {
        let values = json!({"host":{"image":"registry/host@sha256:abc"},
            "roles":{"http":{"image":"registry/http@sha256:def"}},
            "extra":[{"image":"registry/extra@sha256:fed"}]});
        let mut images = BTreeMap::new();
        image_references(&values, "platform", &mut images);
        assert_eq!(images["platform/host"], values["host"]["image"]);
        assert_eq!(
            images["platform/roles/http"],
            values["roles"]["http"]["image"]
        );
        assert_eq!(images["platform/extra/0"], values["extra"][0]["image"]);
        for (path, image) in images {
            assert_eq!(
                values
                    .pointer(&format!("{}/image", path.strip_prefix("platform").unwrap()))
                    .unwrap(),
                &image
            );
        }
        let selected = selected_chart(Ok(Some(
            &json!({"platform":{"chart":"/nonexistent/dep-08a-selected-chart"}}),
        )));
        assert_eq!(selected.category, Category::Unknown);
        assert!(selected.images.is_empty());
        assert_eq!(selected_chart(Err(())).category, Category::Unknown);
        assert_eq!(
            selected_chart(Ok(Some(&json!({})))).category,
            Category::Unknown
        );
    }

    #[test]
    fn endpoint_slices_report_only_ready_nonterminating_addresses() {
        let mut slices = json!({"items":[{"addressType":"IPv4","ports":[{"name":"http","port":80}],
            "endpoints":[{"addresses":["10.0.0.7"],"conditions":{"ready":true,"terminating":false}}]}]});
        assert_eq!(
            endpoints("hosts/Service/control", Some(&slices)).category,
            Category::Ready
        );
        slices["items"][0]["endpoints"][0]["conditions"]["terminating"] = json!(true);
        assert_eq!(
            endpoints("hosts/Service/control", Some(&slices)).category,
            Category::Incomplete
        );
        assert_eq!(
            endpoints("hosts/Service/control", None).category,
            Category::Unknown
        );
    }

    #[test]
    fn installed_baseline_is_not_judged_against_the_status_executable() {
        use crate::provision_system::SystemState;
        use wamn_package_control::schema_baseline::{InitializationIdentity, SchemaReport};
        let state = SystemState::BaselineMismatch {
            installed: "installed-hash".into(),
            expected: "local-executable-hash".into(),
        };
        let mut report = SchemaReport {
            initialization: Some(InitializationIdentity {
                sha256: "installed-hash".into(),
                installed_at: "original-identity".into(),
            }),
            installed: vec![],
            pending: vec![],
            uncertain: vec![],
            missing: vec![],
            warnings: vec![],
        };
        let observed = database_state(&state, &report);
        assert_eq!(observed.category, Category::Ready);
        let identity = report.initialization.as_ref().unwrap();
        assert!(observed.facts.iter().any(|fact| {
            fact.contains(&identity.sha256) && fact.contains(&identity.installed_at)
        }));
        report.initialization = None;
        assert_eq!(
            database_state(&SystemState::Absent, &report).category,
            Category::Incomplete
        );
        assert_eq!(
            database_state(&SystemState::Incomplete, &report).category,
            Category::Incomplete
        );
    }

    #[test]
    fn past_job_pods_do_not_override_current_service_readiness() {
        let pods = json!({"items":[
            {"kind":"Pod","metadata":{"name":"bootstrap-old","ownerReferences":[{"kind":"Job"}]},"status":{"phase":"Failed"}},
            {"kind":"Pod","metadata":{"name":"identity-current","ownerReferences":[{"kind":"ReplicaSet"}]},
                "status":{"phase":"Running","conditions":[{"type":"Ready","status":"True"}]}}
        ]});
        let mut observations = Vec::new();
        append_resources(&mut observations, "identity/pods", Some(&pods));
        assert_eq!(
            combined(observations.iter().map(|item| item.category)),
            Category::Ready
        );
        assert!(
            observations
                .iter()
                .any(|item| item.source.ends_with("identity-current"))
        );
    }

    #[test]
    fn status_output_excludes_environment_values_and_controller_messages() {
        let sensitive = "private-database-password";
        let object = json!({"kind":"Deployment","metadata":{"generation":1},
            "spec":{"replicas":1,"template":{"spec":{"containers":[{"name":"host","image":"registry/host@sha256:abc",
                "env":[{"name":"DATABASE_URL","value":sensitive}]}]}}},
            "status":{"observedGeneration":1,"conditions":[{"type":"Progressing","status":"False",
                "reason":"ProgressDeadlineExceeded","message":sensitive}]}});
        let status = PlatformStatus {
            category: Category::Failed,
            targets: vec![TargetStatus {
                name: "control".into(),
                selected: selected_chart(Err(())),
                category: Category::Failed,
                warnings: vec![],
                observations: vec![resource("hosts/Deployment/control", Ok(Some(&object)))],
            }],
            observations: vec![],
            warnings: vec![],
        };
        let mut output = Vec::new();
        write(&status, &mut output).unwrap();
        assert!(!String::from_utf8(output).unwrap().contains(sensitive));
    }
}
