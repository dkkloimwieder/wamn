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
// env apply passes the policy's 600 s readiness budget to Helm. Allow 900 s
// here so Helm can report its own timeout and finish its cleanup first.
const INSTALL_DEADLINE: Duration = Duration::from_mins(15);
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

/// A failure reported by an install Job's status.
#[derive(Debug)]
pub(super) struct JobError {
    pub kind: JobErrorKind,
    pub name: String,
}

impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            JobErrorKind::JobFailed => write!(f, "install Job {} failed", self.name),
        }
    }
}

impl std::error::Error for JobError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum JobErrorKind {
    JobFailed,
}

/// Inputs to one install Job. Arguments contain no credentials.
#[derive(Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent property of one Job"
)]
pub struct JobRequest {
    pub name: String,
    pub image: String,
    pub arguments: Vec<String>,
    pub calls_kubernetes: bool,
    /// An existing account, for example the platform's Workload Identity binding.
    pub service_account: Option<String>,
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
    /// Existing Secrets mounted read-only in this Job's namespace.
    pub secret_mounts: Vec<SecretMount>,
    /// Credential preparation before the native sidecar starts.
    pub setup: Option<JobContainer>,
    /// One native sidecar from the same runner image.
    pub sidecar: Option<JobContainer>,
    /// The command refuses a second run, so a completed Job never reruns for
    /// a new image (wamn-xjna8).
    pub once: bool,
    /// Read changing live state, or perform an action guarded by that state.
    pub refresh: bool,
}

/// Add the existing auth NATS inputs, copied under `/inputs` or mounted in place.
pub(super) fn auth_nats_inputs(
    request: &mut JobRequest,
    copied: Option<&super::install::SecretData>,
) -> anyhow::Result<()> {
    let directory = if let Some(secret) = copied {
        for key in ["ca.crt", "tls.crt", "tls.key"] {
            request
                .files
                .insert(key.into(), super::install::secret_text(secret, key)?);
        }
        request.environment.insert(
            "WAMN_AUTH_NATS_TLS_SERVER_NAME".into(),
            "nats.platform.svc.cluster.local".into(),
        );
        "/inputs"
    } else {
        request.secret_mounts.push(SecretMount {
            name: "wasmcloud-runtime-tls".into(),
            path: "/etc/wamn/auth-nats".into(),
            keys: ["ca.crt", "tls.crt", "tls.key"].map(str::to_owned).into(),
        });
        "/etc/wamn/auth-nats"
    };
    request.environment.insert(
        "WAMN_AUTH_NATS_URL".into(),
        "nats://nats.platform.svc.cluster.local:4222".into(),
    );
    for (variable, key) in [
        ("WAMN_AUTH_NATS_TLS_CA", "ca.crt"),
        ("WAMN_AUTH_NATS_TLS_CERT", "tls.crt"),
        ("WAMN_AUTH_NATS_TLS_KEY", "tls.key"),
    ] {
        request
            .environment
            .insert(variable.into(), format!("{directory}/{key}"));
    }
    Ok(())
}

/// A read-only projection of an existing Secret. Values stay in the cluster.
#[derive(Debug, Clone, Serialize)]
pub struct SecretMount {
    pub name: String,
    pub path: String,
    pub keys: Vec<String>,
}

/// Another container in the Job pod, using the main container's image.
#[derive(Debug, Default, Serialize)]
pub struct JobContainer {
    pub command: Vec<String>,
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
    /// Environment name to Secret name and key, without any credential value.
    pub secret_environment: BTreeMap<String, (String, String)>,
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
    init_containers: Vec<Container>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    restart_policy: Option<&'static str>,
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct VolumeMount {
    name: String,
    mount_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    read_only: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Volume {
    name: String,
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
            name: "out".into(),
            empty_dir: Some(EmptyDir {
                medium: "Memory",
                size_limit: "1Mi",
            }),
            secret: None,
        });
        volume_mounts.push(VolumeMount {
            name: "out".into(),
            mount_path: "/out".into(),
            read_only: None,
        });
    }
    if !request.files.is_empty() || !request.binary_files.is_empty() {
        volumes.push(Volume {
            name: "inputs".into(),
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
            name: "inputs".into(),
            mount_path: "/inputs".into(),
            read_only: None,
        });
    }
    // Companions receive the file inputs, never the operator certificate.
    let companion_mounts = volume_mounts.clone();
    for (index, mount) in request.secret_mounts.iter().enumerate() {
        let name = format!("existing-{index}");
        volumes.push(Volume {
            name: name.clone(),
            empty_dir: None,
            secret: Some(SecretVolume {
                secret_name: mount.name.clone(),
                default_mode: 0o400,
                items: mount
                    .keys
                    .iter()
                    .map(|key| SecretItem {
                        key: key.clone(),
                        path: key.clone(),
                    })
                    .collect(),
            }),
        });
        volume_mounts.push(VolumeMount {
            name,
            mount_path: mount.path.clone(),
            read_only: Some(true),
        });
    }
    let mut init_containers = Vec::new();
    for (name, companion, restart_policy) in [
        ("prepare", request.setup.as_ref(), None),
        ("gate", request.sidecar.as_ref(), Some("Always")),
    ] {
        if let Some(companion) = companion {
            let mut env: Vec<_> = companion
                .environment
                .iter()
                .map(|(name, value)| Environment {
                    name: name.clone(),
                    value: Some(value.clone()),
                    value_from: None,
                })
                .collect();
            env.extend(
                companion
                    .secret_environment
                    .iter()
                    .map(|(name, (secret, key))| Environment {
                        name: name.clone(),
                        value: None,
                        value_from: Some(SecretSource {
                            secret_key_ref: SecretKey {
                                name: secret.clone(),
                                key: key.clone(),
                            },
                        }),
                    }),
            );
            init_containers.push(Container {
                name: name.into(),
                image: request.image.clone(),
                restart_policy,
                command: companion.command.clone(),
                args: companion.arguments.clone(),
                env,
                volume_mounts: companion_mounts.clone(),
            });
        }
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
                    service_account_name: request
                        .service_account
                        .clone()
                        .or_else(|| request.calls_kubernetes.then(|| request.name.clone())),
                    security_context: request
                        .run_as_user
                        .map(|run_as_user| SecurityContext { run_as_user }),
                    init_containers,
                    containers: vec![Container {
                        restart_policy: None,
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
    (request.calls_kubernetes && request.service_account.is_none())
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

/// The completion digest of a Job. A `once` Job leaves its image out, so a new
/// image does not rerun it.
fn completion_digest(request: &JobRequest, inputs: &Value) -> anyhow::Result<String> {
    let image = if request.once {
        ""
    } else {
        request.image.as_str()
    };
    let mut digest_inputs = json!({
        "inputs": inputs, "image": image, "arguments": request.arguments,
        "environment": request.environment, "files": request.files,
        "binary_files": request.binary_files,
        "permissions": request.permissions, "command": request.command,
        "capture_output": request.capture_output, "run_as_user": request.run_as_user
    });
    if !request.secret_mounts.is_empty() {
        digest_inputs["secret_mounts"] = serde_json::to_value(&request.secret_mounts)?;
    }
    if request.setup.is_some() || request.sidecar.is_some() {
        digest_inputs["setup"] = serde_json::to_value(&request.setup)?;
        digest_inputs["sidecar"] = serde_json::to_value(&request.sidecar)?;
    }
    if let Some(account) = &request.service_account {
        digest_inputs["service_account"] = account.clone().into();
    }
    Ok(input_digest(&digest_inputs))
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
    let digest = completion_digest(request, inputs)?;
    let marker = format!("wamn-install-{}", request.name);
    let completed = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "platform",
        "configmap",
        &marker,
    )
    .await?;
    if !request.refresh && skip(completed.as_ref(), &digest) {
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
                    name: request
                        .service_account
                        .as_ref()
                        .unwrap_or(&request.name)
                        .clone(),
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
        let deadline = Instant::now() + INSTALL_DEADLINE;
        loop {
            // A dropped connection says nothing about the Job, which keeps
            // running, so the poll retries it until the deadline.
            let status: Value = match kubectl::get(
                &target.kubeconfig,
                &target.context,
                &target.namespace,
                "job",
                &request.name,
            )
            .await
            {
                Err(error) if error.is::<kubectl::Unreachable>() && Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
                status => status?.context("install Job disappeared")?,
            };
            if let Some(conditions) = status["status"]["conditions"].as_array() {
                if conditions
                    .iter()
                    .any(|condition| condition["type"] == "Failed" && condition["status"] == "True")
                {
                    return Err(JobError {
                        kind: JobErrorKind::JobFailed,
                        name: request.name.clone(),
                    }
                    .into());
                }
                if conditions.iter().any(|condition| {
                    condition["type"] == "Complete" && condition["status"] == "True"
                }) {
                    break Ok(());
                }
            }
            ensure!(
                Instant::now() < deadline,
                "install Job {} did not complete in {}s",
                request.name,
                INSTALL_DEADLINE.as_secs()
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    .await;
    let cleanup = finish(target, work_directory, request).await;
    let output = cleanup?;
    result.with_context(|| {
        let path = work_directory
            .join("jobs")
            .join(format!("{}.log", request.name));
        let log = std::fs::read_to_string(&path).unwrap_or_default();
        format!("install Job log saved at {}:\n{log}", path.display())
    })?;
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
    let output = if request.capture_output && request.sidecar.is_some() {
        Some(
            kubectl::kubectl(
                &target.kubeconfig,
                &target.context,
                &target.namespace,
                &[
                    "logs",
                    &format!("job/{name}"),
                    "--container",
                    name,
                    "--pod-running-timeout=10s",
                ],
                None,
            )
            .await,
        )
    } else {
        None
    };
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
        output.unwrap_or(log).context("read the public Job result")
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
/// Build install requests without contacting Kubernetes, databases, or brokers.
#[cfg(feature = "test-util")]
pub fn install_job_requests(
    registry: &wamn_package_artifact::PackageRegistry,
) -> anyhow::Result<Vec<(&'static str, JobRequest, Vec<JobSecret>)>> {
    use super::{control, environments, identity, install, invite, packages, releases, web};
    let document = serde_json::from_value(json!({
        "provider": "kind", "project_id": "", "zone": "", "cluster_name": "test",
        "org": "test", "owner_email": "owner@example.com", "registry": "registry",
        "buckets": {"web": "test-web", "otel": "", "state": ""},
        "route_hosts": {"control": "control.example.com"}, "resend_from": "sender@example.com"
    }))?;
    let environment = serde_json::from_value(json!({
        "org": "test", "project": "receiving", "env": "dev", "target": "dev",
        "release": format!("sha256:{}", "a".repeat(64)),
        "route_host": "test.example.com", "policy": "dev", "applications": {}
    }))?;
    let auth = serde_json::from_value(json!({"data": {
        "ca.crt": STANDARD.encode("test-ca"), "tls.crt": STANDARD.encode("test-cert"),
        "tls.key": STANDARD.encode("test-key")
    }}))?;
    let image = "registry/runner@sha256:abc";
    let tenant = "test--receiving--dev";
    let mut requests = Vec::new();
    for request in install::provisioning_jobs(&document, image) {
        let verb = match request.name.as_str() {
            "platform-system" => "provision-system",
            "platform-org" => "provision-org",
            _ => "provision-identity-issuer",
        };
        requests.push((verb, request, vec!["WAMN_SYSTEM_ADMIN_URL"]));
    }
    requests.extend([
        (
            "provision-org",
            control::provision_request(&document, image),
            vec![],
        ),
        ("invite", invite::request(&document, image, &auth)?, vec![]),
        (
            "provision-project-env",
            identity::session_request(&environment, image),
            vec!["WAMN_SYSTEM_ADMIN_URL", "WAMN_TARGET_ADMIN_DATABASE_URL"],
        ),
        (
            "apply-package",
            packages::package_request(
                tenant,
                "fixture-1".into(),
                &environment,
                image,
                registry,
                &auth,
            )?,
            vec!["WAMN_SYSTEM_ADMIN_URL", "WAMN_PG_ADMIN_URL"],
        ),
        (
            "reconcile-package-data-access",
            packages::access_request(tenant, &environment, image, registry),
            vec!["WAMN_SYSTEM_ADMIN_URL", "WAMN_PG_ADMIN_URL"],
        ),
        (
            "reconcile-replica-identity",
            packages::replica_request(tenant, "fixture", image),
            vec!["WAMN_SYSTEM_ADMIN_URL", "WAMN_PG_ADMIN_URL"],
        ),
        (
            "publish-release",
            releases::request(tenant, "", image, registry)?,
            vec!["WAMN_SYSTEM_ADMIN_URL", "WAMN_PG_ADMIN_URL"],
        ),
        (
            "web upload",
            web::request(&environment, "test-web", image, registry)?,
            vec![],
        ),
        (
            "web upload --platform-ui",
            web::platform_ui_request(&document, image),
            vec![],
        ),
    ]);
    for initial in [true, false] {
        requests.push((
            "env apply",
            environments::request(&document, &environment, image, initial, &auth)?,
            vec!["WAMN_CONTROL_DATABASE_URL"],
        ));
    }
    Ok(requests
        .into_iter()
        .map(|(verb, request, variables)| {
            let secrets = variables
                .into_iter()
                .map(|variable| JobSecret {
                    name: "test-credential".into(),
                    key: "url".into(),
                    value: "private".into(),
                    environment: variable.into(),
                })
                .collect();
            (verb, request, secrets)
        })
        .collect())
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
    fn a_once_job_keeps_its_completion_across_a_new_image() {
        let mut first = request();
        let mut second = request();
        second.image = "other/runner@sha256:def".into();
        let inputs = json!({});
        assert_ne!(
            completion_digest(&first, &inputs).unwrap(),
            completion_digest(&second, &inputs).unwrap()
        );
        first.once = true;
        second.once = true;
        assert_eq!(
            completion_digest(&first, &inputs).unwrap(),
            completion_digest(&second, &inputs).unwrap()
        );
        second.arguments.push("--other".into());
        assert_ne!(
            completion_digest(&first, &inputs).unwrap(),
            completion_digest(&second, &inputs).unwrap()
        );
    }

    #[test]
    fn native_sidecar_and_setup_use_the_same_image_and_scoped_secrets() {
        let mut request = request();
        request.secret_mounts.push(SecretMount {
            name: "operator-test".into(),
            path: "/etc/wamn/operator".into(),
            keys: vec!["tls.crt".into(), "tls.key".into()],
        });
        request.setup = Some(JobContainer {
            command: vec!["/bin/sh".into()],
            arguments: vec!["-ec".into(), "prepare".into()],
            ..JobContainer::default()
        });
        request.sidecar = Some(JobContainer {
            command: vec!["/usr/local/bin/wamn-scenario-worker".into()],
            arguments: vec!["serve".into()],
            environment: [("WAMN_MANAGEMENT_ORG".into(), "test".into())].into(),
            secret_environment: [(
                "WAMN_SYSTEM_URL".into(),
                ("gate-reader".into(), "url".into()),
            )]
            .into(),
        });
        let job = serde_json::to_value(render(&request, "identity", &[])).unwrap();
        let pod = &job["spec"]["template"]["spec"];
        assert_eq!(job["metadata"]["namespace"], "identity");
        assert_eq!(pod["initContainers"].as_array().unwrap().len(), 2);
        assert!(pod["initContainers"][0].get("restartPolicy").is_none());
        assert_eq!(pod["initContainers"][1]["restartPolicy"], "Always");
        assert_eq!(pod["initContainers"][1]["image"], request.image);
        assert_eq!(pod["initContainers"][1]["args"], json!(["serve"]));
        assert_eq!(
            pod["initContainers"][1]["env"][1]["valueFrom"]["secretKeyRef"],
            json!({"name":"gate-reader","key":"url"})
        );
        assert!(pod["initContainers"][1].get("volumeMounts").is_none());
        assert_eq!(pod["containers"][0]["volumeMounts"][0]["readOnly"], true);
        assert_eq!(pod["volumes"][0]["secret"]["secretName"], "operator-test");
        assert!(pod["containers"][0].get("restartPolicy").is_none());
        let ordinary =
            serde_json::to_value(render(&super::tests::request(), "platform", &[])).unwrap();
        assert!(
            ordinary["spec"]["template"]["spec"]
                .get("initContainers")
                .is_none()
        );
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
    fn existing_account_is_used_without_creating_or_owning_it() {
        let mut request = request();
        request.calls_kubernetes = true;
        request.service_account = Some("wamn-host".into());
        let owner = metadata(&request.name, "hosts");
        assert!(account(&request, &owner).unwrap().is_none());
        let job = serde_json::to_value(render(&request, "hosts", &[])).unwrap();
        let pod = &job["spec"]["template"]["spec"];
        assert_eq!(pod["serviceAccountName"], "wamn-host");
        assert_eq!(pod["automountServiceAccountToken"], true);
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
        let auth = serde_json::from_value(json!({"data": {
            "ca.crt": STANDARD.encode("private-mounted-ca"),
            "tls.crt": STANDARD.encode("private-mounted-cert"),
            "tls.key": STANDARD.encode("private-mounted-key")
        }}))
        .unwrap();
        auth_nats_inputs(&mut request, Some(&auth)).unwrap();
        let job = serde_json::to_value(render(&request, "platform", &[])).unwrap();
        assert!(!job.to_string().contains("private-mounted-key"));
        assert!(!format!("{request:?}").contains("private-mounted-key"));
        assert_eq!(
            job["spec"]["template"]["spec"]["volumes"][0]["secret"],
            json!({"secretName":"platform-system-files","defaultMode":256,
                "items":[{"key":"file-0","path":"ca.crt"},{"key":"file-1","path":"tls.crt"},{"key":"file-2","path":"tls.key"}]})
        );
        assert_eq!(
            job["spec"]["template"]["spec"]["containers"][0]["env"][2],
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
