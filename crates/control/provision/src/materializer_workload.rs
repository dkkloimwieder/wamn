//! The event materializer's workload, rendered from
//! `deploy/platform/materializer.example.yaml`.
//!
//! The cluster files, the cluster tests and the development loop render the
//! one template with [`render_materializer`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_yaml::Value;

/// How often the deployed materializer fetches from its consumers.
pub const FETCH_MS: u64 = 1000;
/// How often the deployed materializer sweeps its pending runs.
pub const SWEEP_MS: u64 = 5000;

/// Event coordinates declared by the environment owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventIdentity {
    pub org: String,
    pub project: String,
    pub environment: String,
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

/// Render the materializer while retaining its native binding and retry limit.
pub fn render_materializer(
    template: &str,
    input: &MaterializerInput,
) -> Result<String, serde_yaml::Error> {
    let mut workload: MaterializerDocument = serde_yaml::from_str(template)?;
    workload.metadata.name.clone_from(&input.workload);
    workload.metadata.namespace.clone_from(&input.namespace);
    let spec = &mut workload.spec.template.spec;
    spec.environment.clone_from(&input.namespace);
    let service = &mut spec.service;
    service.image.clone_from(&input.image);
    let config = &mut service.local_resources.config;
    config.tenant.clone_from(&input.tenant);
    config.project.clone_from(&input.event.project);
    config.environment.clone_from(&input.event.environment);
    let environment = &mut service.local_resources.environment.config;
    environment.stream.clone_from(&input.event_stream);
    environment.org.clone_from(&input.event.org);
    environment.project.clone_from(&input.event.project);
    environment.environment.clone_from(&input.event.environment);
    environment.tenant.clone_from(&input.tenant);
    environment.fetch_ms = Some(input.fetch_ms.to_string());
    environment.sweep_ms = Some(input.sweep_ms.to_string());
    serde_yaml::to_string(&workload)
}

type Extra = BTreeMap<String, Value>;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Metadata {
    name: String,
    namespace: String,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MaterializerDocument {
    api_version: String,
    kind: WorkloadKind,
    metadata: Metadata,
    spec: DeploymentSpec,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum WorkloadKind {
    WorkloadDeployment,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeploymentSpec {
    replicas: u32,
    template: WorkloadTemplate,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkloadTemplate {
    spec: MaterializerWorkload,
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

#[cfg(test)]
mod tests {
    use super::*;

    const MATERIALIZER: &str =
        include_str!("../../../../deploy/platform/materializer.example.yaml");

    fn materializer_input() -> MaterializerInput {
        MaterializerInput {
            workload: "fixture-materializer".into(),
            namespace: "warehouse-eu-3".into(),
            image: "registry.test.invalid:5000/materializer@sha256:abc123".into(),
            tenant: "fixture-route-auth".into(),
            event: EventIdentity {
                org: "example".into(),
                project: "fixture".into(),
                environment: "dev".into(),
            },
            event_stream: "EVT_7_example_7_fixture_3_dev".into(),
            fetch_ms: 500,
            sweep_ms: 500,
        }
    }

    #[test]
    fn materializer_renders_each_identity_and_preserves_native_binding_and_retry() {
        let first = materializer_input();
        let second = MaterializerInput {
            workload: "hopper-materializer".into(),
            tenant: "quay-9-route-auth".into(),
            event: EventIdentity {
                org: "zamboni".into(),
                project: "quay9".into(),
                environment: "stage7".into(),
            },
            event_stream: "EVT_7_zamboni_5_quay9_6_stage7".into(),
            fetch_ms: 131,
            sweep_ms: 137,
            ..first.clone()
        };
        let original: MaterializerDocument = serde_yaml::from_str(MATERIALIZER).unwrap();
        for input in [first, second] {
            let rendered = render_materializer(MATERIALIZER, &input).unwrap();
            let document: MaterializerDocument = serde_yaml::from_str(&rendered).unwrap();
            assert_eq!(document.metadata.name, input.workload);
            assert_eq!(document.metadata.namespace, input.namespace);
            let spec = &document.spec.template.spec;
            assert_eq!(spec.environment, input.namespace);
            assert_eq!(spec.service.image, input.image);
            let claims = &spec.service.local_resources.config;
            assert_eq!(claims.tenant, input.tenant);
            assert_eq!(claims.project, input.event.project);
            assert_eq!(claims.environment, input.event.environment);
            assert_eq!(claims.authority, MaterializerAuthority::EventMaterializer);
            let environment = &spec.service.local_resources.environment.config;
            assert_eq!(environment.stream, input.event_stream);
            assert_eq!(environment.org, input.event.org);
            assert_eq!(environment.project, input.event.project);
            assert_eq!(environment.environment, input.event.environment);
            assert_eq!(environment.tenant, input.tenant);
            assert_eq!(
                environment.fetch_ms.as_deref(),
                Some(input.fetch_ms.to_string().as_str())
            );
            assert_eq!(
                environment.sweep_ms.as_deref(),
                Some(input.sweep_ms.to_string().as_str())
            );
            assert_eq!(environment.max_deliver, "5");
            assert_eq!(
                spec.host_interfaces,
                original.spec.template.spec.host_interfaces
            );
            let native = spec
                .host_interfaces
                .iter()
                .find(|interface| interface.namespace == "wasmcloud" && interface.package == "nats")
                .unwrap();
            assert_eq!(native.name.as_deref(), Some("events"));
            assert_eq!(native.interfaces, ["types", "jetstream"]);
            let registration = spec
                .host_interfaces
                .iter()
                .find(|interface| interface.namespace == "wamn" && interface.package == "jetstream")
                .unwrap();
            assert_eq!(registration.interfaces, ["types", "registration"]);
        }
    }

    #[test]
    fn materializer_refuses_missing_identity_extra_subscription_fields_and_credentials() {
        let input = materializer_input();
        for field in [
            "WAMN_MAT_STREAM",
            "WAMN_MAT_ORG",
            "WAMN_MAT_PROJECT",
            "WAMN_MAT_ENV",
            "WAMN_MAT_TENANT",
        ] {
            let mut document: Value = serde_yaml::from_str(MATERIALIZER).unwrap();
            document["spec"]["template"]["spec"]["service"]["localResources"]["environment"]["config"]
                .as_mapping_mut()
                .unwrap()
                .remove(Value::from(field));
            assert!(
                format!(
                    "{:#}",
                    render_materializer(&serde_yaml::to_string(&document).unwrap(), &input)
                        .unwrap_err()
                )
                .contains(field)
            );
        }
        for field in [
            "WAMN_MAT_LEGACY_TENANT",
            "WAMN_EVT_NATS_PASSWORD",
            "WAMN_MAT_NATS_BINDING_FILE",
        ] {
            let mut document: Value = serde_yaml::from_str(MATERIALIZER).unwrap();
            document["spec"]["template"]["spec"]["service"]["localResources"]["environment"]["config"]
                [field] = Value::from("t1");
            assert!(
                format!(
                    "{:#}",
                    render_materializer(&serde_yaml::to_string(&document).unwrap(), &input)
                        .unwrap_err()
                )
                .contains(field)
            );
        }
    }

    #[test]
    fn materializer_refuses_missing_required_fields() {
        let input = materializer_input();
        for path in [
            vec!["metadata", "name"],
            vec!["metadata", "namespace"],
            vec!["spec", "template", "spec", "environment"],
            vec!["spec", "template", "spec", "service", "image"],
            vec![
                "spec",
                "template",
                "spec",
                "service",
                "localResources",
                "config",
                "wamn.tenant",
            ],
            vec![
                "spec",
                "template",
                "spec",
                "service",
                "localResources",
                "config",
                "wamn.project",
            ],
            vec![
                "spec",
                "template",
                "spec",
                "service",
                "localResources",
                "config",
                "wamn.environment",
            ],
        ] {
            let mut document: Value = serde_yaml::from_str(MATERIALIZER).unwrap();
            let mut parent = &mut document;
            for part in &path[..path.len() - 1] {
                parent = &mut parent[*part];
            }
            parent
                .as_mapping_mut()
                .unwrap()
                .remove(Value::from(*path.last().unwrap()));
            assert!(
                render_materializer(&serde_yaml::to_string(&document).unwrap(), &input).is_err(),
                "{path:?}"
            );
        }
    }
}
