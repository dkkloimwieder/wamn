//! Run install Jobs with temporary owned Secrets and a digest of their inputs.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{kubectl, release_chart::Target};

const OUTPUT_FILE: &str = "/out/secret.json";
const HANDOFF: &str = r#"umask 077
/usr/local/bin/wamn-ctl "$@"
waited=0
while [ -e /out/secret.json ] && [ "$waited" -lt 300 ]; do
  sleep 1
  waited=$((waited + 1))
done
test ! -e /out/secret.json
"#;
const READ_OUTPUT: &str = r#"waited=0
while [ ! -f /out/secret.json ] && [ "$waited" -lt 300 ]; do
  sleep 1
  waited=$((waited + 1))
done
cat /out/secret.json && rm /out/secret.json
"#;

/// Inputs to one install Job. Arguments contain no credentials.
#[derive(Default)]
pub struct JobRequest {
    pub name: String,
    pub image: String,
    pub arguments: Vec<String>,
    pub calls_kubernetes: bool,
    /// The command writes its Secret to `/out/secret.json`.
    pub emits_secret: bool,
    /// Public environment inputs. Credentials use `JobSecret`.
    pub environment: BTreeMap<String, String>,
    /// Private mounted inputs, keyed by filename under `/inputs`.
    pub files: BTreeMap<String, String>,
    /// Binary mounted inputs, such as the chart's packaged dependency.
    pub binary_files: BTreeMap<String, Vec<u8>>,
    /// Namespace-scoped access required by this Job.
    pub permissions: Vec<Access>,
    /// Override the executable for an existing tool, such as identity or psql.
    pub command: Vec<String>,
    /// Retain public command output with the completion digest.
    pub capture_output: bool,
    /// UID for tools whose image user cannot read the private input volume.
    pub run_as_user: Option<u32>,
}

impl std::fmt::Debug for JobRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobRequest")
            .field("name", &self.name)
            .field("image", &self.image)
            .field("arguments", &self.arguments)
            .finish_non_exhaustive()
    }
}

/// Kubernetes permissions for one namespace.
#[derive(Debug, Serialize)]
pub struct Access {
    pub namespace: String,
    pub rules: Vec<PolicyRule>,
}

/// One Kubernetes authorization rule.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyRule {
    pub api_groups: Vec<String>,
    pub resources: Vec<String>,
    pub verbs: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Role {
    api_version: &'static str,
    kind: &'static str,
    metadata: Metadata,
    rules: Vec<PolicyRule>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RoleBinding {
    api_version: &'static str,
    kind: &'static str,
    metadata: Metadata,
    role_ref: RoleRef,
    subjects: Vec<Subject>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RoleRef {
    api_group: &'static str,
    kind: &'static str,
    name: String,
}

#[derive(Serialize)]
struct Subject {
    kind: &'static str,
    name: String,
    namespace: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Metadata {
    name: String,
    namespace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    uid: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    owner_references: Vec<OwnerReference>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    labels: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct Created {
    metadata: Metadata,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OwnerReference {
    api_version: String,
    kind: String,
    name: String,
    uid: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Job {
    api_version: &'static str,
    kind: &'static str,
    metadata: Metadata,
    spec: JobSpec,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct JobSpec {
    backoff_limit: u32,
    template: PodTemplate,
}

#[derive(Debug, Serialize)]
struct PodTemplate {
    spec: PodSpec,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PodSpec {
    restart_policy: &'static str,
    automount_service_account_token: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    service_account_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    security_context: Option<SecurityContext>,
    containers: Vec<Container>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    volumes: Vec<Volume>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SecurityContext {
    run_as_user: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Container {
    name: String,
    image: String,
    command: Vec<String>,
    args: Vec<String>,
    env: Vec<Environment>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    volume_mounts: Vec<VolumeMount>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Environment {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    value_from: Option<SecretSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SecretSource {
    secret_key_ref: SecretKey,
}

#[derive(Debug, Serialize)]
struct SecretKey {
    name: String,
    key: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct VolumeMount {
    name: &'static str,
    mount_path: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Volume {
    name: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    empty_dir: Option<EmptyDir>,
    #[serde(skip_serializing_if = "Option::is_none")]
    secret: Option<SecretVolume>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SecretVolume {
    secret_name: String,
    default_mode: u32,
    items: Vec<SecretItem>,
}

#[derive(Debug, Serialize)]
struct SecretItem {
    key: String,
    path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EmptyDir {
    medium: &'static str,
    size_limit: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServiceAccount {
    api_version: &'static str,
    kind: &'static str,
    metadata: Metadata,
}

/// One temporary Secret key exposed as an environment variable to the Job.
pub struct JobSecret {
    pub name: String,
    pub key: String,
    pub value: String,
    pub environment: String,
}

impl std::fmt::Debug for JobSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobSecret")
            .field("name", &self.name)
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Secret {
    api_version: String,
    kind: String,
    metadata: Metadata,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    string_data: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    data: BTreeMap<String, String>,
    #[serde(rename = "type", default)]
    content_type: String,
}

/// A completed step's input digest, stored in the platform namespace.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Completion {
    api_version: String,
    kind: String,
    metadata: Metadata,
    #[serde(default)]
    data: BTreeMap<String, String>,
}

/// SHA-256 over canonical JSON, independent of object key order.
pub fn input_digest(inputs: &Value) -> String {
    hex::encode(
        ring::digest::digest(
            &ring::digest::SHA256,
            &wamn_execution_contract::canonical_json_bytes(inputs),
        )
        .as_ref(),
    )
}

/// Skip only a step with the same completed input digest.
pub fn skip(completion: Option<&Completion>, digest: &str) -> bool {
    completion.is_some_and(|value| value.data.get("digest").is_some_and(|old| old == digest))
}

fn metadata(name: &str, namespace: &str) -> Metadata {
    Metadata {
        name: name.into(),
        namespace: namespace.into(),
        ..Metadata::default()
    }
}

fn owned_metadata(name: &str, owner: &Metadata) -> anyhow::Result<Metadata> {
    Ok(Metadata {
        owner_references: vec![OwnerReference {
            api_version: "batch/v1".into(),
            kind: "Job".into(),
            name: owner.name.clone(),
            uid: owner.uid.clone().context("created Job has no uid")?,
        }],
        ..metadata(name, &owner.namespace)
    })
}

fn render(request: &JobRequest, namespace: &str, secrets: &[JobSecret]) -> Job {
    let mut args = request.arguments.clone();
    let mut command = if request.command.is_empty() {
        vec!["/usr/local/bin/wamn-ctl".into()]
    } else {
        request.command.clone()
    };
    let mut volumes = Vec::new();
    let mut volume_mounts = Vec::new();
    if request.emits_secret {
        command = vec!["/bin/sh".into(), "-ec".into()];
        args.splice(0..0, [HANDOFF.into(), request.name.clone()]);
        volumes.push(Volume {
            name: "out",
            empty_dir: Some(EmptyDir {
                medium: "Memory",
                size_limit: "1Mi",
            }),
            secret: None,
        });
        volume_mounts.push(VolumeMount {
            name: "out",
            mount_path: "/out",
        });
    }
    if !request.files.is_empty() || !request.binary_files.is_empty() {
        volumes.push(Volume {
            name: "inputs",
            empty_dir: None,
            secret: Some(SecretVolume {
                secret_name: format!("{}-files", request.name),
                default_mode: 0o400,
                items: request
                    .files
                    .keys()
                    .chain(request.binary_files.keys())
                    .enumerate()
                    .map(|(index, path)| SecretItem {
                        key: format!("file-{index}"),
                        path: path.clone(),
                    })
                    .collect(),
            }),
        });
        volume_mounts.push(VolumeMount {
            name: "inputs",
            mount_path: "/inputs",
        });
    }
    Job {
        api_version: "batch/v1",
        kind: "Job",
        metadata: metadata(&request.name, namespace),
        spec: JobSpec {
            backoff_limit: 0,
            template: PodTemplate {
                spec: PodSpec {
                    restart_policy: "Never",
                    automount_service_account_token: request.calls_kubernetes,
                    service_account_name: request.calls_kubernetes.then(|| request.name.clone()),
                    security_context: request
                        .run_as_user
                        .map(|run_as_user| SecurityContext { run_as_user }),
                    containers: vec![Container {
                        name: request.name.clone(),
                        image: request.image.clone(),
                        command,
                        args,
                        env: secrets
                            .iter()
                            .map(|secret| Environment {
                                name: secret.environment.clone(),
                                value_from: Some(SecretSource {
                                    secret_key_ref: SecretKey {
                                        name: secret.name.clone(),
                                        key: secret.key.clone(),
                                    },
                                }),
                                value: None,
                            })
                            .chain(request.environment.iter().map(|(name, value)| Environment {
                                name: name.clone(),
                                value: Some(value.clone()),
                                value_from: None,
                            }))
                            .collect(),
                        volume_mounts,
                    }],
                    volumes,
                },
            },
        },
    }
}

fn account(request: &JobRequest, owner: &Metadata) -> anyhow::Result<Option<ServiceAccount>> {
    request
        .calls_kubernetes
        .then(|| {
            Ok(ServiceAccount {
                api_version: "v1",
                kind: "ServiceAccount",
                metadata: owned_metadata(&request.name, owner)?,
            })
        })
        .transpose()
}

fn owned_secret(secret: &JobSecret, owner: &Metadata) -> anyhow::Result<Secret> {
    Ok(Secret {
        api_version: "v1".into(),
        kind: "Secret".into(),
        metadata: owned_metadata(&secret.name, owner)?,
        string_data: [(secret.key.clone(), secret.value.clone())].into(),
        data: BTreeMap::new(),
        content_type: "Opaque".into(),
    })
}

/// Run a Job, retain its log, remove its temporary objects, and record completion.
///
/// # Errors
///
/// Returns the first execution error. Cleanup errors also refuse completion.
pub async fn run(
    target: &Target,
    work_directory: &Path,
    request: &JobRequest,
    inputs: &Value,
    secrets: &[JobSecret],
) -> anyhow::Result<bool> {
    let digest = input_digest(&json!({
        "inputs": inputs, "image": request.image, "arguments": request.arguments,
        "environment": request.environment, "files": request.files,
        "binary_files": request.binary_files,
        "permissions": request.permissions, "command": request.command,
        "capture_output": request.capture_output, "run_as_user": request.run_as_user
    }));
    let marker = format!("wamn-install-{}", request.name);
    let completed = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "platform",
        "configmap",
        &marker,
    )
    .await?;
    if skip(completed.as_ref(), &digest) {
        return Ok(false);
    }
    let existing: Option<Value> = kubectl::get(
        &target.kubeconfig,
        &target.context,
        &target.namespace,
        "job",
        &request.name,
    )
    .await?;
    if existing.is_some() {
        finish(target, work_directory, request).await?;
    }
    let rendered = render(request, &target.namespace, secrets);
    let input = serde_json::to_vec(&rendered)?;
    let created = kubectl::kubectl(
        &target.kubeconfig,
        &target.context,
        &target.namespace,
        &["create", "-f", "-", "-o", "json"],
        Some(&input),
    )
    .await?;
    let created: Created = serde_json::from_str(&created).context("decode the created Job")?;
    let result = async {
        if let Some(account) = account(request, &created.metadata)? {
            kubectl::apply(
                &target.kubeconfig,
                &target.context,
                &target.namespace,
                &account,
            )
            .await?;
        }
        for access in &request.permissions {
            // A namespaced Job cannot own objects in another namespace. These
            // Roles and bindings are deleted explicitly with the Job.
            let role = Role {
                api_version: "rbac.authorization.k8s.io/v1",
                kind: "Role",
                metadata: metadata(&request.name, &access.namespace),
                rules: access
                    .rules
                    .iter()
                    .map(|rule| PolicyRule {
                        api_groups: rule.api_groups.clone(),
                        resources: rule.resources.clone(),
                        verbs: rule.verbs.clone(),
                    })
                    .collect(),
            };
            kubectl::apply(
                &target.kubeconfig,
                &target.context,
                &access.namespace,
                &role,
            )
            .await?;
            let binding = RoleBinding {
                api_version: "rbac.authorization.k8s.io/v1",
                kind: "RoleBinding",
                metadata: metadata(&request.name, &access.namespace),
                role_ref: RoleRef {
                    api_group: "rbac.authorization.k8s.io",
                    kind: "Role",
                    name: request.name.clone(),
                },
                subjects: vec![Subject {
                    kind: "ServiceAccount",
                    name: request.name.clone(),
                    namespace: target.namespace.clone(),
                }],
            };
            kubectl::apply(
                &target.kubeconfig,
                &target.context,
                &access.namespace,
                &binding,
            )
            .await?;
        }
        for secret in secrets {
            kubectl::apply(
                &target.kubeconfig,
                &target.context,
                &target.namespace,
                &owned_secret(secret, &created.metadata)?,
            )
            .await?;
        }
        if !request.files.is_empty() || !request.binary_files.is_empty() {
            let secret = Secret {
                api_version: "v1".into(),
                kind: "Secret".into(),
                metadata: owned_metadata(&format!("{}-files", request.name), &created.metadata)?,
                string_data: request
                    .files
                    .values()
                    .enumerate()
                    .map(|(index, value)| (format!("file-{index}"), value.clone()))
                    .collect(),
                data: request
                    .binary_files
                    .values()
                    .enumerate()
                    .map(|(index, bytes)| {
                        (
                            format!("file-{}", request.files.len() + index),
                            STANDARD.encode(bytes),
                        )
                    })
                    .collect(),
                content_type: "Opaque".into(),
            };
            kubectl::apply(
                &target.kubeconfig,
                &target.context,
                &target.namespace,
                &secret,
            )
            .await?;
        }
        if request.emits_secret {
            kubectl::wait(
                &target.kubeconfig,
                &target.context,
                &target.namespace,
                "job",
                &request.name,
                "jsonpath={.status.active}=1",
                "300s",
            )
            .await?;
            kubectl::kubectl(
                &target.kubeconfig,
                &target.context,
                &target.namespace,
                &[
                    "wait",
                    "pod",
                    "-l",
                    &format!("job-name={}", request.name),
                    "--for=condition=Ready",
                    "--timeout=300s",
                ],
                None,
            )
            .await?;
            let output = kubectl::kubectl(
                &target.kubeconfig,
                &target.context,
                &target.namespace,
                &[
                    "exec",
                    &format!("job/{}", request.name),
                    "--",
                    "/bin/sh",
                    "-ec",
                    READ_OUTPUT,
                ],
                Some(&[]),
            )
            .await?;
            // Parser diagnostics can quote the credential. Keep them out of errors.
            let secret: Secret = serde_json::from_str(&output)
                .map_err(|_| anyhow::anyhow!("decode the emitted Secret from {OUTPUT_FILE}"))?;
            ensure!(
                secret.api_version == "v1" && secret.kind == "Secret",
                "the Job output is not a v1 Secret"
            );
            kubectl::apply(
                &target.kubeconfig,
                &target.context,
                &secret.metadata.namespace,
                &secret,
            )
            .await?;
        }
        let deadline = Instant::now() + Duration::from_secs(600);
        loop {
            let status: Value = kubectl::get(
                &target.kubeconfig,
                &target.context,
                &target.namespace,
                "job",
                &request.name,
            )
            .await?
            .context("install Job disappeared")?;
            if let Some(conditions) = status["status"]["conditions"].as_array() {
                ensure!(
                    !conditions
                        .iter()
                        .any(|condition| condition["type"] == "Failed"
                            && condition["status"] == "True"),
                    "install Job {} failed",
                    request.name
                );
                if conditions.iter().any(|condition| {
                    condition["type"] == "Complete" && condition["status"] == "True"
                }) {
                    break Ok(());
                }
            }
            ensure!(
                Instant::now() < deadline,
                "install Job {} did not complete in 600s",
                request.name
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    .await;
    let cleanup = finish(target, work_directory, request).await;
    let output = cleanup?;
    result?;
    let mut data: BTreeMap<_, _> = [("digest".into(), digest)].into();
    if request.capture_output {
        data.insert("output".into(), output);
    }
    kubectl::apply(
        &target.kubeconfig,
        &target.context,
        "platform",
        &Completion {
            api_version: "v1".into(),
            kind: "ConfigMap".into(),
            metadata: metadata(&marker, "platform"),
            data,
        },
    )
    .await?;
    Ok(true)
}

async fn finish(target: &Target, directory: &Path, request: &JobRequest) -> anyhow::Result<String> {
    let name = &request.name;
    let log = kubectl::kubectl(
        &target.kubeconfig,
        &target.context,
        &target.namespace,
        &[
            "logs",
            &format!("job/{name}"),
            "--all-containers=true",
            "--pod-running-timeout=10s",
        ],
        None,
    )
    .await;
    let directory = directory.join("jobs");
    std::fs::create_dir_all(&directory).context("create the Job log directory")?;
    let saved = std::fs::write(
        directory.join(format!("{name}.log")),
        match &log {
            Ok(log) => log.clone(),
            Err(error) => format!("Job log unavailable: {error:#}\n"),
        },
    )
    .context("save the Job log");
    saved?;
    // After its log is saved, even a failed Job must release its credentials.
    let deleted = kubectl::kubectl(
        &target.kubeconfig,
        &target.context,
        &target.namespace,
        &[
            "delete",
            "job",
            name,
            "--ignore-not-found",
            "--cascade=foreground",
            "--wait=true",
            "--timeout=120s",
        ],
        None,
    )
    .await;
    deleted?;
    for access in &request.permissions {
        kubectl::kubectl(
            &target.kubeconfig,
            &target.context,
            &access.namespace,
            &["delete", "role,rolebinding", name, "--ignore-not-found"],
            None,
        )
        .await?;
    }
    if request.capture_output {
        log.context("read the public Job result")
    } else {
        Ok(String::new())
    }
}

/// Read the public result retained by a completed Job.
///
/// # Errors
///
/// Returns an error when the completion or its result is absent.
pub async fn output(target: &Target, request: &JobRequest) -> anyhow::Result<String> {
    let marker = format!("wamn-install-{}", request.name);
    let completion: Completion = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "platform",
        "configmap",
        &marker,
    )
    .await?
    .context("the Job has no completion")?;
    completion
        .data
        .get("output")
        .cloned()
        .context("the Job has no public result")
}
#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> JobRequest {
        JobRequest {
            name: "platform-system".into(),
            image: "registry/runner@sha256:abc".into(),
            arguments: vec![
                "provision-system".into(),
                "--platform-domain".into(),
                "example.com".into(),
            ],
            calls_kubernetes: false,
            emits_secret: false,
            ..JobRequest::default()
        }
    }

    #[test]
    fn job_uses_the_runner_and_only_its_owned_secret() {
        let request = request();
        let secret = JobSecret {
            name: "wamn-system-admin".into(),
            key: "url".into(),
            value: "private".into(),
            environment: "WAMN_SYSTEM_ADMIN_URL".into(),
        };
        let job = serde_json::to_value(render(&request, "platform", &[secret])).unwrap();
        assert_eq!(job["apiVersion"], "batch/v1");
        assert_eq!(job["spec"]["backoffLimit"], 0);
        let pod = &job["spec"]["template"]["spec"];
        assert_eq!(pod["restartPolicy"], "Never");
        assert_eq!(pod["automountServiceAccountToken"], false);
        assert!(pod.get("serviceAccountName").is_none());
        assert_eq!(
            pod["containers"][0]["command"],
            json!(["/usr/local/bin/wamn-ctl"])
        );
        assert_eq!(pod["containers"][0]["args"], json!(request.arguments));
        assert_eq!(pod["containers"][0]["image"], request.image);
        assert_eq!(
            pod["containers"][0]["env"][0]["valueFrom"]["secretKeyRef"],
            json!({"name":"wamn-system-admin","key":"url"})
        );
        assert!(!job.to_string().contains("private"));
    }

    #[test]
    fn account_and_secret_name_the_created_jobs_uid() {
        let mut request = request();
        let mut owner = metadata(&request.name, "platform");
        owner.uid = Some("live-uid".into());
        assert!(account(&request, &owner).unwrap().is_none());
        request.calls_kubernetes = true;
        let account = serde_json::to_value(account(&request, &owner).unwrap().unwrap()).unwrap();
        let secret = JobSecret {
            name: "admin".into(),
            key: "url".into(),
            value: "private".into(),
            environment: "ADMIN".into(),
        };
        let secret = serde_json::to_value(owned_secret(&secret, &owner).unwrap()).unwrap();
        for object in [&account, &secret] {
            assert_eq!(
                object["metadata"]["ownerReferences"],
                json!([{
                    "apiVersion":"batch/v1","kind":"Job","name":"platform-system","uid":"live-uid"
                }])
            );
            assert_eq!(object["metadata"]["namespace"], "platform");
        }
        assert_eq!(account["kind"], "ServiceAccount");
        assert_eq!(secret["stringData"]["url"], "private");
        let job = serde_json::to_value(render(&request, "platform", &[])).unwrap();
        assert_eq!(
            job["spec"]["template"]["spec"]["serviceAccountName"],
            "platform-system"
        );
        owner.uid = None;
        assert!(
            owned_secret(
                &JobSecret {
                    name: "admin".into(),
                    key: "url".into(),
                    value: "private".into(),
                    environment: "ADMIN".into()
                },
                &owner
            )
            .is_err()
        );
    }

    #[test]
    fn emitted_secret_uses_memory_and_a_bounded_handoff() {
        let mut request = request();
        request.emits_secret = true;
        let job = serde_json::to_value(render(&request, "platform", &[])).unwrap();
        let pod = &job["spec"]["template"]["spec"];
        assert_eq!(
            pod["volumes"],
            json!([{"name":"out","emptyDir":{"medium":"Memory","sizeLimit":"1Mi"}}])
        );
        assert_eq!(pod["containers"][0]["args"][0], HANDOFF);
        assert_eq!(pod["containers"][0]["args"][1], request.name);
        assert_eq!(pod["containers"][0]["args"][2], "provision-system");
    }

    #[test]
    fn mounted_credentials_never_enter_the_job_or_debug_output() {
        let mut request = request();
        request
            .files
            .insert("tls.key".into(), "private-mounted-key".into());
        request
            .environment
            .insert("WAMN_AUTH_NATS_TLS_KEY".into(), "/inputs/tls.key".into());
        let job = serde_json::to_value(render(&request, "platform", &[])).unwrap();
        assert!(!job.to_string().contains("private-mounted-key"));
        assert!(!format!("{request:?}").contains("private-mounted-key"));
        assert_eq!(
            job["spec"]["template"]["spec"]["volumes"][0]["secret"],
            json!({"secretName":"platform-system-files","defaultMode":256,
                "items":[{"key":"file-0","path":"tls.key"}]})
        );
        assert_eq!(
            job["spec"]["template"]["spec"]["containers"][0]["env"][0],
            json!({"name":"WAMN_AUTH_NATS_TLS_KEY","value":"/inputs/tls.key"})
        );
    }

    #[test]
    fn nested_chart_and_package_paths_project_from_valid_secret_keys() {
        let mut request = request();
        request.files.insert(
            "package/migrations/0001_initial.sql".into(),
            "private-sql".into(),
        );
        request
            .binary_files
            .insert("charts/runtime-operator.tgz".into(), vec![0, 255]);
        let job = serde_json::to_value(render(&request, "platform", &[])).unwrap();
        assert_eq!(
            job["spec"]["template"]["spec"]["volumes"][0]["secret"]["items"],
            json!([
                {"key":"file-0","path":"package/migrations/0001_initial.sql"},
                {"key":"file-1","path":"charts/runtime-operator.tgz"}
            ])
        );
        assert!(!job.to_string().contains("private-sql"));
        assert!(!format!("{request:?}").contains("private-sql"));
    }

    #[test]
    fn emitted_identity_secret_keeps_its_labels_and_credential() {
        let output = json!({
            "apiVersion":"v1", "kind":"Secret", "type":"Opaque",
            "metadata":{"name":"wamn-identity-db","namespace":"identity",
                "labels":{"app.kubernetes.io/name":"wamn-identity","app.kubernetes.io/managed-by":"wamn"}},
            "stringData":{"url":"private"}
        });
        let secret: Secret = serde_json::from_value(output.clone()).unwrap();
        assert_eq!(serde_json::to_value(secret).unwrap(), output);
    }

    #[test]
    fn digest_is_canonical_and_skip_requires_the_same_inputs() {
        assert_eq!(
            input_digest(&json!({})),
            "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
        );
        let a: Value =
            serde_json::from_str(r#"{"version":"1","document":{"b":2,"a":1},"image":"one"}"#)
                .unwrap();
        let b: Value =
            serde_json::from_str(r#"{"image":"one","document":{"a":1,"b":2},"version":"1"}"#)
                .unwrap();
        let digest = input_digest(&a);
        assert_eq!(digest, input_digest(&b));
        let mut completion = Completion {
            api_version: "v1".into(),
            kind: "ConfigMap".into(),
            metadata: metadata("step", "platform"),
            data: [("digest".into(), digest.clone())].into(),
        };
        assert!(skip(Some(&completion), &digest));
        assert!(!skip(None, &digest));
        for field in ["document", "version", "image"] {
            let mut changed = a.clone();
            changed[field] = json!("changed");
            assert!(!skip(Some(&completion), &input_digest(&changed)));
        }
        completion.data.clear();
        assert!(!skip(Some(&completion), &digest));
    }
}
