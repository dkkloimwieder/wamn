//! Server-authorized application Jobs and bounded Kubernetes observations.

use anyhow::Context as _;
use serde::Deserialize;
use serde_json::{Value, json};
use wamn_catalog::HostAttachment;
use wamn_identity_protocol::PrincipalId;
use wamn_kubernetes::kubernetes_api::KubernetesApi;

use super::{ControlRoutes, Refusal};

const NAMESPACE: &str = "platform";
const TARGET: &str = "wamn.dev/operation-target";
const OPERATION: &str = "wamn.dev/operation";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    project: String,
    env: String,
    #[serde(default)]
    application: Option<String>,
    #[serde(default)]
    release: Option<String>,
    #[serde(default)]
    resolve: Option<String>,
    #[serde(default)]
    job: Option<String>,
}

#[derive(Debug, Clone, Copy)]
enum Operation {
    Deploy,
    Rollback,
    Diff,
    Apply,
    Reset,
    Adopt,
}

impl Operation {
    fn parse(operation: &str) -> Option<Self> {
        Some(match operation {
            "application.deploy" => Self::Deploy,
            "application.rollback" => Self::Rollback,
            "schema.diff" => Self::Diff,
            "schema.apply" => Self::Apply,
            "schema.reset" => Self::Reset,
            "schema.adopt" => Self::Adopt,
            _ => return None,
        })
    }

    fn command(self) -> &'static str {
        match self {
            Self::Deploy => "deploy",
            Self::Rollback => "rollback",
            Self::Diff => "diff",
            Self::Apply => "apply",
            Self::Reset => "reset",
            Self::Adopt => "adopt",
        }
    }

    fn privileged(self) -> bool {
        matches!(self, Self::Apply | Self::Reset | Self::Adopt)
    }

    fn needs(
        self,
        org: &str,
        cluster: &str,
        metadata: bool,
    ) -> Vec<(String, &'static str, &'static str)> {
        let mut needs = vec![
            ("wamn-apply".into(), "apply", "/credentials/apply"),
            ("wamn-ctl-nats".into(), "nats", "/credentials/nats"),
        ];
        if !metadata {
            needs.push((
                format!("wamn-operation-registry-{org}"),
                "registry",
                "/credentials/registry",
            ));
        }
        if self.privileged() {
            needs.push((
                format!("{cluster}-superuser"),
                "administrator",
                "/credentials/administrator",
            ));
        }
        needs
    }
}

fn invalid(field: &'static str) -> Refusal {
    Refusal::Declared {
        code: "invalid_input",
        detail: json!({"field":field}),
    }
}

impl ControlRoutes<'_> {
    pub(super) async fn operation_job(
        &self,
        attachment: &HostAttachment,
        caller: &PrincipalId,
        payload: &str,
    ) -> Result<Value, Refusal> {
        let request: Request = serde_json::from_str(payload).map_err(|_| invalid("value"))?;
        let coordinate = wamn_control_provision::environment_names::environment_resource_name(
            self.org,
            &request.project,
            &request.env,
        )
        .map_err(|_| invalid("env"))?;
        // Repeat authority in the write transaction before contacting Kubernetes.
        let mut writer = self.writer.lock().await;
        let transaction = self
            .begin_in(&mut writer, attachment, caller, Some(&request.project))
            .await?;
        let exists = transaction
            .query_opt(
                "SELECT 1 FROM registry.project_envs WHERE org=$1 AND project=$2 AND env=$3",
                &[&self.org, &request.project, &request.env],
            )
            .await?
            .is_some();
        if !exists {
            return Err(Refusal::Declared {
                code: "environment_not_found",
                detail: json!({"field":"env"}),
            });
        }
        transaction.commit().await?;
        drop(writer);

        let api;
        let api = if let Some(api) = self.operation_jobs {
            api
        } else {
            api = KubernetesApi::in_cluster().map_err(anyhow::Error::from)?;
            &api
        };
        if attachment.route.operation == "operation.status" {
            if request.application.is_some()
                || request.release.is_some()
                || request.resolve.is_some()
            {
                return Err(invalid("value"));
            }
            return status(api, &request, &coordinate).await;
        }
        let operation =
            Operation::parse(attachment.route.operation).context("unknown operation Job route")?;
        validate(operation, &request)?;
        let environment = api
            .get(format!("/api/v1/namespaces/hosts/configmaps/{coordinate}"))
            .await
            .map_err(anyhow::Error::from)?;
        let environment: Value = serde_json::from_str(
            environment["data"]["environment.json"]
                .as_str()
                .context("canonical environment document is absent")?,
        )
        .context("decode canonical environment")?;
        if environment["org"] != self.org
            || environment["project"] != request.project
            || environment["env"] != request.env
        {
            return Err(invalid("env"));
        }
        if let Some(application) = &request.application
            && environment["applications"].get(application).is_none()
        {
            return Err(invalid("application"));
        }
        // Both ConfigMaps are installed by trusted platform/environment provisioning.
        let configuration = api
            .get(format!(
                "/api/v1/namespaces/{NAMESPACE}/configmaps/wamn-operation-{}",
                self.org
            ))
            .await
            .map_err(anyhow::Error::from)?;
        let inputs = configuration["data"]["inputs"]
            .as_str()
            .filter(|name| {
                kubernetes_name(name) && name.starts_with(&format!("wamn-operation-{}-", self.org))
            })
            .context("worker input selection is absent")?;
        let configuration = api
            .get(format!(
                "/api/v1/namespaces/{NAMESPACE}/configmaps/{inputs}"
            ))
            .await
            .map_err(anyhow::Error::from)?;
        let target = api
            .get(format!(
                "/api/v1/namespaces/{NAMESPACE}/configmaps/{coordinate}-operations"
            ))
            .await
            .map_err(anyhow::Error::from)?;
        let cluster = target["data"]["cluster"]
            .as_str()
            .filter(|name| kubernetes_name(name))
            .context("operation target has no CNPG cluster")?;
        let image = configuration["data"]["image"]
            .as_str()
            .context("operation image is absent")?;
        let (_, digest) = image
            .rsplit_once("@sha256:")
            .context("operation image is not pinned")?;
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(anyhow::anyhow!("operation image digest is invalid").into());
        }
        let name = format!("op-{}", uuid::Uuid::new_v4());
        let job = manifest(
            operation,
            &request,
            self.org,
            caller.to_string().as_str(),
            &coordinate,
            cluster,
            image,
            &name,
            inputs,
            self.registry_token_metadata,
        );
        let created = api
            .create_job(NAMESPACE, &job)
            .await
            .map_err(anyhow::Error::from)?;
        if created["metadata"]["name"] != name {
            return Err(anyhow::anyhow!("Kubernetes returned a different Job").into());
        }
        Ok(json!({"job":name}))
    }
}

fn validate(operation: Operation, request: &Request) -> Result<(), Refusal> {
    if request.job.is_some() {
        return Err(invalid("job"));
    }
    if let Some(application) = &request.application
        && (application.is_empty()
            || application.len() > 253
            || application.contains(['\0', '\n', '\r']))
    {
        return Err(invalid("application"));
    }
    if let Some(release) = &request.release {
        let valid = release.strip_prefix("sha256:").is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        });
        if !valid {
            return Err(invalid("release"));
        }
    }
    if request
        .resolve
        .as_ref()
        .is_some_and(|value| value.len() > 1024 || value.contains(['\0', '\n', '\r']))
    {
        return Err(invalid("resolve"));
    }
    match operation {
        Operation::Deploy if request.release.is_none() || request.application.is_some() => {
            return Err(invalid("release"));
        }
        Operation::Rollback | Operation::Apply | Operation::Diff
            if request.application.is_none() =>
        {
            return Err(invalid("application"));
        }
        Operation::Reset if request.application.is_some() || request.release.is_some() => {
            return Err(invalid("application"));
        }
        Operation::Adopt if request.release.is_some() => return Err(invalid("release")),
        _ => {}
    }
    if !matches!(operation, Operation::Apply) && request.resolve.is_some() {
        return Err(invalid("resolve"));
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "all inputs are server-resolved manifest fields"
)]
fn manifest(
    operation: Operation,
    request: &Request,
    org: &str,
    actor: &str,
    coordinate: &str,
    cluster: &str,
    image: &str,
    name: &str,
    inputs: &str,
    metadata: bool,
) -> Value {
    let mut arguments = vec![
        "operation",
        operation.command(),
        "--org",
        org,
        "--project",
        request.project.as_str(),
        "--env",
        request.env.as_str(),
    ];
    for (flag, value) in [
        ("--application", request.application.as_deref()),
        ("--release", request.release.as_deref()),
        ("--resolve", request.resolve.as_deref()),
    ] {
        if let Some(value) = value {
            arguments.extend([flag, value]);
        }
    }
    let mut volumes = vec![
        json!({"name":"inputs", "configMap":{"name":inputs}}),
        json!({"name":"work", "emptyDir":{}}),
    ];
    let mut mounts = vec![
        json!({"name":"inputs","mountPath":"/operation-inputs","readOnly":true}),
        json!({"name":"work","mountPath":"/work"}),
    ];
    for (secret, volume, path) in operation.needs(org, cluster, metadata) {
        volumes.push(json!({"name":volume,"secret":{"secretName":secret}}));
        mounts.push(json!({"name":volume,"mountPath":path,"readOnly":true}));
    }
    json!({"apiVersion":"batch/v1", "kind":"Job",
        "metadata":{"name":name, "namespace":NAMESPACE, "annotations":{TARGET:coordinate,OPERATION:operation.command()}},
        "spec":{"backoffLimit":0,"activeDeadlineSeconds":900,"ttlSecondsAfterFinished":3600,
            "template":{"spec":{"serviceAccountName":format!("{coordinate}-{}",if operation.privileged() {"sql"} else {"code"}),
                "automountServiceAccountToken":true,"restartPolicy":"Never", "volumes":volumes,
                "containers":[{"name":"operation","image":image,"command":["/usr/local/bin/wamn-ctl"],"args":arguments,
                    "env":[{"name":"WAMN_DEPLOY_ACTOR","value":actor},
                        {"name":"KUBECONFIG","value":"/work/kubeconfig"},
                        {"name":"WAMN_RELEASE_CHART","value":"/work/chart"},
                        if metadata {
                            json!({"name":"WAMN_REGISTRY_TOKEN_METADATA","value":"true"})
                        } else {
                            json!({"name":"WAMN_REGISTRY_AUTH_FILE","value":"/credentials/registry/config.json"})
                        },
                        {"name":"WAMN_NATS_URL","value":"nats://wamn-nats.platform.svc.cluster.local:4222"},
                        {"name":"WAMN_NATS_TLS_CA","value":"/credentials/nats/ca.crt"},
                        {"name":"WAMN_NATS_TLS_CERT","value":"/credentials/nats/tls.crt"},
                        {"name":"WAMN_NATS_TLS_KEY","value":"/credentials/nats/tls.key"},
                        {"name":"WAMN_APPLY_DATABASE_URL","valueFrom":{"secretKeyRef":{"name":"wamn-apply","key":"url"}}},
                        {"name":"WAMN_RELEASE_ARTIFACT_BASE","valueFrom":{"configMapKeyRef":{"name":inputs,"key":"release-base"}}}],
                    "volumeMounts":mounts,"resources":{"requests":{"cpu":"100m","memory":"256Mi"},"limits":{"cpu":"2","memory":"2Gi"}}}]}}}})
}

fn kubernetes_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name.as_bytes()[name.len() - 1].is_ascii_alphanumeric()
}

async fn status(
    api: &KubernetesApi,
    request: &Request,
    coordinate: &str,
) -> Result<Value, Refusal> {
    let name = request.job.as_deref().ok_or_else(|| invalid("job"))?;
    if !kubernetes_name(name)
        || name
            .strip_prefix("op-")
            .is_none_or(|id| uuid::Uuid::parse_str(id).is_err())
    {
        return Err(invalid("job"));
    }
    let job = api
        .get_job(NAMESPACE, name)
        .await
        .map_err(anyhow::Error::from)?
        .ok_or_else(|| Refusal::Declared {
            code: "not_found",
            detail: json!({"field":"job"}),
        })?;
    if job["metadata"]["annotations"][TARGET] != coordinate
        || !matches!(
            job["metadata"]["annotations"][OPERATION].as_str(),
            Some("deploy" | "rollback" | "diff" | "apply" | "reset" | "adopt")
        )
    {
        return Err(Refusal::Declared {
            code: "not_found",
            detail: json!({"field":"job"}),
        });
    }
    let uid = job["metadata"]["uid"]
        .as_str()
        .context("Job UID is absent")?;
    let uid = uuid::Uuid::parse_str(uid).context("Job UID is invalid")?;
    let pods = api.get(format!("/api/v1/namespaces/{NAMESPACE}/pods?labelSelector=batch.kubernetes.io%2Fcontroller-uid%3D{uid}&limit=1")).await.map_err(anyhow::Error::from)?;
    let mut log = String::new();
    if let Some(pod) = pods["items"].as_array().and_then(|items| items.first()) {
        let owned = pod["metadata"]["ownerReferences"]
            .as_array()
            .is_some_and(|owners| {
                owners
                    .iter()
                    .any(|owner| owner["uid"] == uid.to_string() && owner["kind"] == "Job")
            });
        if owned
            && pod["status"]["containerStatuses"]
                .as_array()
                .is_some_and(|states| {
                    states.iter().any(|state| {
                        state["name"] == "operation" && state["state"].get("waiting").is_none()
                    })
                })
        {
            let pod = pod["metadata"]["name"]
                .as_str()
                .filter(|name| kubernetes_name(name))
                .context("pod name is invalid")?;
            log = api
                .pod_log(NAMESPACE, pod)
                .await
                .map_err(anyhow::Error::from)?;
        }
    }
    let conditions = job["status"]["conditions"].as_array();
    let has = |type_| {
        conditions.is_some_and(|conditions| {
            conditions
                .iter()
                .any(|condition| condition["type"] == type_ && condition["status"] == "True")
        })
    };
    let state = if has("Complete") {
        "succeeded"
    } else if has("Failed") {
        "failed"
    } else if job["status"]["active"].as_u64().unwrap_or(0) > 0 {
        "running"
    } else {
        "pending"
    };
    Ok(json!({"job":name,"state":state,"log":log}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn secrets<'a>(value: &'a Value, names: &mut BTreeSet<&'a str>) {
        match value {
            Value::Object(fields) => {
                for (field, value) in fields {
                    let name = match field.as_str() {
                        "secretKeyRef" | "secretRef" => value["name"].as_str(),
                        "secret" => value["secretName"].as_str(),
                        _ => None,
                    };
                    if let Some(name) = name {
                        names.insert(name);
                    }
                    secrets(value, names);
                }
            }
            Value::Array(values) => {
                for value in values {
                    secrets(value, names);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn schema_changes_require_one_application() {
        let mut request: Request = serde_json::from_value(json!({
            "project": "billing", "env": "dev"
        }))
        .unwrap();
        for operation in [Operation::Apply, Operation::Diff] {
            assert!(validate(operation, &request).is_err());
            request.application = Some("billing".into());
            assert!(validate(operation, &request).is_ok());
            request.application = None;
        }
        assert!(validate(Operation::Reset, &request).is_ok());
    }

    #[test]
    fn job_credentials_follow_declared_operation_needs() {
        let request = Request {
            project: "billing".into(),
            env: "dev".into(),
            application: None,
            release: None,
            resolve: None,
            job: None,
        };
        for metadata in [false, true] {
            for (_, attachment) in
                wamn_catalog::HostRouteSet::Control
                    .attachments()
                    .filter(|(_, attachment)| {
                        attachment.route.handler == wamn_catalog::HostHandler::OperationJob
                            && attachment.route.operation != "operation.status"
                    })
            {
                let route = attachment.route;
                let operation = Operation::parse(route.operation)
                    .expect("every Job route declares its operation needs");
                let job = manifest(
                    operation,
                    &request,
                    "org-a",
                    "authenticated-actor",
                    "coordinate",
                    "cnpg",
                    "registry/ctl@sha256:abc",
                    "op-test",
                    "selected-inputs",
                    metadata,
                );
                let mut actual = BTreeSet::new();
                secrets(&job, &mut actual);
                let needs = operation.needs("org-a", "cnpg", metadata);
                let expected: BTreeSet<_> =
                    needs.iter().map(|(name, _, _)| name.as_str()).collect();
                assert_eq!(actual, expected, "{}", route.operation);
                assert_eq!(actual.contains("wamn-operation-registry-org-a"), !metadata);
                let variables = job["spec"]["template"]["spec"]["containers"][0]["env"]
                    .as_array()
                    .unwrap();
                assert_eq!(
                    variables.iter().any(
                        |v| v["name"] == "WAMN_REGISTRY_TOKEN_METADATA" && v["value"] == "true"
                    ),
                    metadata
                );
                assert_eq!(
                    variables
                        .iter()
                        .any(|v| v["name"] == "WAMN_REGISTRY_AUTH_FILE"),
                    !metadata
                );
                match operation {
                    Operation::Deploy | Operation::Rollback | Operation::Diff => {
                        assert!(!actual.contains("cnpg-superuser"));
                    }
                    Operation::Apply | Operation::Reset | Operation::Adopt => {
                        assert!(actual.contains("cnpg-superuser"));
                    }
                }
                assert_eq!(
                    job["spec"]["template"]["spec"]["containers"][0]["env"][0]["value"],
                    "authenticated-actor"
                );
            }
        }
    }
}
