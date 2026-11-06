//! Run short bootstrap Jobs with temporary owned credentials and public results.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::kubectl;
use wamn_release_control::release_chart::Target;

const OUTPUT_FILE: &str = "/out/secret.json";
// Bound bootstrap execution even if the calling process exits.
const INSTALL_DEADLINE: Duration = Duration::from_mins(15);
pub(super) const HANDOFF: &str = r#"umask 077
/usr/local/bin/wamn-ctl "$@"
waited=0
while [ -e /out/secret.json ] && [ "$waited" -lt 300 ]; do
  sleep 1
  waited=$((waited + 1))
done
test ! -e /out/secret.json
"#;
const READ_OUTPUT: &str = "if [ -f /out/secret.json ]; then cat /out/secret.json; fi";
const ACK_OUTPUT: &str = "rm -f /out/secret.json";

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
    /// Override the executable for an existing tool, such as identity or psql.
    pub command: Vec<String>,
    /// UID for tools whose image user cannot read the private input volume.
    pub run_as_user: Option<u32>,
    /// Existing Secrets mounted read-only in this Job's namespace.
    pub secret_mounts: Vec<SecretMount>,
}

/// A read-only projection of an existing Secret. Values stay in the cluster.
#[derive(Debug, Clone, Serialize)]
pub struct SecretMount {
    pub name: String,
    pub path: String,
    pub keys: Vec<String>,
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
    suspend: bool,
    backoff_limit: u32,
    active_deadline_seconds: u64,
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

/// A Job request and its temporary credentials.
pub(super) type JobInputs = (JobRequest, Vec<JobSecret>);

/// Public command output. Emitted Secret data never enters this result.
#[derive(Debug)]
pub struct JobResult {
    pub stdout: String,
}

/// One temporary Secret key exposed as an environment variable to the Job.
#[derive(Clone)]
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
    if !request.files.is_empty() {
        volumes.push(Volume {
            name: "inputs".into(),
            empty_dir: None,
            secret: Some(SecretVolume {
                secret_name: format!("{}-files", request.name),
                default_mode: 0o400,
                items: request
                    .files
                    .keys()
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
    Job {
        api_version: "batch/v1",
        kind: "Job",
        metadata: metadata(&request.name, namespace),
        spec: JobSpec {
            suspend: true,
            backoff_limit: 0,
            active_deadline_seconds: INSTALL_DEADLINE.as_secs(),
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

/// Run a Job and return its public output after cleanup.
///
/// Existing Jobs are collected before a new request runs. Interrupted handoffs
/// remain available for recovery until their Kubernetes deadline expires.
///
/// # Errors
/// Returns execution, handoff, log, or cleanup failures.
pub async fn run(
    target: &Target,
    directory: &Path,
    request: &JobRequest,
    secrets: &[JobSecret],
) -> anyhow::Result<JobResult> {
    super::job_contract::request(request)?;
    let rendered = render(request, &target.namespace, secrets);
    let existing: Option<Value> = kubectl::get(
        &target.kubeconfig,
        &target.context,
        &target.namespace,
        "job",
        &request.name,
    )
    .await?;
    if let Some(existing) = existing {
        let container = &existing["spec"]["template"]["spec"]["containers"][0];
        let previous = JobRequest {
            name: request.name.clone(),
            command: serde_json::from_value(container["command"].clone())?,
            arguments: serde_json::from_value(container["args"].clone())?,
            emits_secret: container["args"][0] == HANDOFF,
            ..JobRequest::default()
        };
        // A suspended Job never started. Its support objects can be incomplete.
        if existing["spec"]["suspend"] == true {
            cleanup(target, &previous).await?;
        } else {
            collect(target, directory, &previous).await?;
        }
    }
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
    let prepared = async {
        for object in support_objects(request, &created.metadata, secrets)? {
            let namespace = object["metadata"]["namespace"]
                .as_str()
                .context("Job support namespace")?;
            kubectl::apply(&target.kubeconfig, &target.context, namespace, &object).await?;
        }
        anyhow::Ok(())
    }
    .await;
    if let Err(error) = prepared {
        return match cleanup(target, request).await {
            Ok(()) => Err(error),
            Err(cleanup) => Err(error.context(format!("Job cleanup failed: {cleanup:#}"))),
        };
    }
    kubectl::kubectl(
        &target.kubeconfig,
        &target.context,
        &target.namespace,
        &[
            "patch",
            "job",
            &request.name,
            "--type=merge",
            "--patch",
            r#"{"spec":{"suspend":false}}"#,
        ],
        None,
    )
    .await?;
    collect(target, directory, request).await
}

fn completed(status: &Value, name: &str) -> Result<bool, JobError> {
    let Some(conditions) = status["status"]["conditions"].as_array() else {
        return Ok(false);
    };
    if conditions
        .iter()
        .any(|condition| condition["type"] == "Failed" && condition["status"] == "True")
    {
        return Err(JobError {
            kind: JobErrorKind::JobFailed,
            name: name.into(),
        });
    }
    Ok(conditions
        .iter()
        .any(|condition| condition["type"] == "Complete" && condition["status"] == "True"))
}

async fn wait(target: &Target, request: &JobRequest) -> anyhow::Result<()> {
    let deadline = Instant::now() + INSTALL_DEADLINE;
    let mut handed_off = false;
    loop {
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
            status => status?.context("bootstrap Job disappeared")?,
        };
        if completed(&status, &request.name)? {
            return Ok(());
        }
        if request.emits_secret
            && !handed_off
            && status["status"]["ready"].as_u64().unwrap_or(0) > 0
        {
            handed_off = handoff(target, request).await?;
        }
        ensure!(
            Instant::now() < deadline,
            "bootstrap Job {} did not complete in {}s",
            request.name,
            INSTALL_DEADLINE.as_secs()
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn handoff(target: &Target, request: &JobRequest) -> anyhow::Result<bool> {
    let job = format!("job/{}", request.name);
    let output = kubectl::kubectl(
        &target.kubeconfig,
        &target.context,
        &target.namespace,
        &["exec", &job, "--", "/bin/sh", "-ec", READ_OUTPUT],
        Some(&[]),
    )
    .await?;
    if output.is_empty() {
        return Ok(false);
    }
    let secret = emitted_secret(&output)?;
    kubectl::apply(
        &target.kubeconfig,
        &target.context,
        &secret.metadata.namespace,
        &secret,
    )
    .await?;
    kubectl::kubectl(
        &target.kubeconfig,
        &target.context,
        &target.namespace,
        &["exec", &job, "--", "/bin/sh", "-ec", ACK_OUTPUT],
        Some(&[]),
    )
    .await?;
    Ok(true)
}

async fn collect(
    target: &Target,
    directory: &Path,
    request: &JobRequest,
) -> anyhow::Result<JobResult> {
    let result = wait(target, request).await;
    let output = logs(target, directory, request).await;
    // An API or handoff failure says nothing about execution. Preserve the Job
    // and its owned credentials so the next invocation can recover its result.
    let cleanup = if result.is_ok() || result.as_ref().is_err_and(anyhow::Error::is::<JobError>) {
        cleanup(target, request).await
    } else {
        Ok(())
    };
    result.with_context(|| {
        let path = directory.join("jobs").join(format!("{}.log", request.name));
        let log = std::fs::read_to_string(&path).unwrap_or_default();
        let cleanup_error = cleanup
            .as_ref()
            .err()
            .map(|error| format!("\nJob cleanup failed: {error:#}"))
            .unwrap_or_default();
        format!(
            "bootstrap Job log saved at {}:\n{log}{cleanup_error}",
            path.display()
        )
    })?;
    cleanup?;
    Ok(JobResult { stdout: output? })
}

fn support_objects(
    request: &JobRequest,
    owner: &Metadata,
    secrets: &[JobSecret],
) -> anyhow::Result<Vec<Value>> {
    let mut objects = Vec::new();
    if let Some(account) = account(request, owner)? {
        objects.push(serde_json::to_value(account)?);
    }
    for access in &super::job_contract::access(request)? {
        objects.push(serde_json::to_value(role(request, access))?);
        objects.push(serde_json::to_value(role_binding(
            request,
            access,
            &owner.namespace,
        ))?);
    }
    for secret in secrets {
        objects.push(serde_json::to_value(owned_secret(secret, owner)?)?);
    }
    if !request.files.is_empty() {
        objects.push(serde_json::to_value(files_secret(request, owner)?)?);
    }
    Ok(objects)
}

fn emitted_secret(output: &str) -> anyhow::Result<Secret> {
    // Parser diagnostics can quote a credential. Keep them out of errors.
    let secret: Secret = serde_json::from_str(output)
        .map_err(|_| anyhow::anyhow!("decode the emitted Secret from {OUTPUT_FILE}"))?;
    ensure!(
        secret.api_version == "v1" && secret.kind == "Secret",
        "the Job output is not a v1 Secret"
    );
    Ok(secret)
}

fn role(request: &JobRequest, access: &Access) -> Role {
    Role {
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
    }
}

fn role_binding(request: &JobRequest, access: &Access, namespace: &str) -> RoleBinding {
    RoleBinding {
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
            namespace: namespace.into(),
        }],
    }
}

fn files_secret(request: &JobRequest, owner: &Metadata) -> anyhow::Result<Secret> {
    Ok(Secret {
        api_version: "v1".into(),
        kind: "Secret".into(),
        metadata: owned_metadata(&format!("{}-files", request.name), owner)?,
        string_data: request
            .files
            .values()
            .enumerate()
            .map(|(index, value)| (format!("file-{index}"), value.clone()))
            .collect(),
        data: BTreeMap::new(),
        content_type: "Opaque".into(),
    })
}

async fn logs(target: &Target, directory: &Path, request: &JobRequest) -> anyhow::Result<String> {
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
            Ok(log) => std::borrow::Cow::Borrowed(log.as_str()),
            Err(error) => std::borrow::Cow::Owned(format!("Job log unavailable: {error:#}\n")),
        }
        .as_bytes(),
    )
    .context("save the Job log");
    saved?;
    log.context("read the public Job result")
}

async fn cleanup(target: &Target, request: &JobRequest) -> anyhow::Result<()> {
    let name = &request.name;
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
    for access in &super::job_contract::access(request)? {
        kubectl::kubectl(
            &target.kubeconfig,
            &target.context,
            &access.namespace,
            &["delete", "role,rolebinding", name, "--ignore-not-found"],
            None,
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        assert_eq!(job["spec"]["suspend"], true);
        assert_eq!(
            job["spec"]["activeDeadlineSeconds"],
            INSTALL_DEADLINE.as_secs()
        );
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
    fn job_account_and_permissions_stay_scoped_to_their_namespaces() {
        let mut request = request();
        request.calls_kubernetes = true;
        request.command = vec!["/bin/sh".into(), "-ec".into()];
        request.arguments = vec![super::super::control::PROVISION.into()];
        let mut owner = metadata(&request.name, "platform");
        owner.uid = Some("live-uid".into());
        let account = account(&request, &owner).unwrap().unwrap();
        assert_eq!(account.metadata.namespace, owner.namespace);
        assert_eq!(account.metadata.owner_references[0].uid, "live-uid");
        for access in super::super::job_contract::access(&request).unwrap() {
            let role = role(&request, &access);
            let binding = role_binding(&request, &access, &owner.namespace);
            assert_eq!(role.kind, "Role");
            assert_eq!(role.metadata.namespace, access.namespace);
            assert_eq!(binding.metadata.namespace, access.namespace);
            assert_eq!(binding.role_ref.kind, role.kind);
            assert_eq!(binding.role_ref.name, role.metadata.name);
            assert_eq!(binding.subjects[0].namespace, account.metadata.namespace);
            assert_eq!(binding.subjects[0].name, account.metadata.name);
        }
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
        request.files = [
            ("ca.crt".into(), "private-mounted-ca".into()),
            ("tls.crt".into(), "private-mounted-cert".into()),
            ("tls.key".into(), "private-mounted-key".into()),
        ]
        .into();
        let job = serde_json::to_value(render(&request, "platform", &[])).unwrap();
        assert!(!job.to_string().contains("private-mounted-key"));
        assert!(!format!("{request:?}").contains("private-mounted-key"));
        assert_eq!(
            job["spec"]["template"]["spec"]["volumes"][0]["secret"],
            json!({"secretName":"platform-system-files","defaultMode":256,
                "items":[{"key":"file-0","path":"ca.crt"},{"key":"file-1","path":"tls.crt"},{"key":"file-2","path":"tls.key"}]})
        );
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
    fn emitted_secret_errors_do_not_expose_private_data() {
        for output in [
            r#"{"private-credential": invalid}"#,
            r#"{"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"private-credential","namespace":"identity"}}"#,
        ] {
            let error = emitted_secret(output).err().unwrap();
            assert!(!format!("{error:#}").contains("private-credential"));
        }
    }

    #[test]
    fn job_conditions_report_failure_and_require_completed_execution() {
        let failed = json!({"status":{"conditions":[{"type":"Failed","status":"True"}]}});
        let error = completed(&failed, "platform-system").unwrap_err();
        assert_eq!(error.kind, JobErrorKind::JobFailed);
        assert_eq!(error.name, "platform-system");
        assert!(!completed(&json!({"status":{"active":1}}), "platform-system").unwrap());
        let mut status = json!({"status":{"conditions":[{"type":"Complete","status":"False"}]}});
        assert!(!completed(&status, "platform-system").unwrap());
        status["status"]["conditions"][0]["status"] = "True".into();
        assert!(completed(&status, "platform-system").unwrap());
    }
}
