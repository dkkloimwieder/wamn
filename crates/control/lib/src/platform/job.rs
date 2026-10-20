//! Run install Jobs with temporary owned Secrets and a digest of their inputs.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context as _, ensure};
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
#[derive(Debug)]
pub struct JobRequest {
    pub name: String,
    pub image: String,
    pub arguments: Vec<String>,
    pub calls_kubernetes: bool,
    /// The command writes its Secret to `/out/secret.json`.
    pub emits_secret: bool,
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
    containers: Vec<Container>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    volumes: Vec<Volume>,
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
    value_from: SecretSource,
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
    empty_dir: EmptyDir,
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
    let mut command = vec!["/usr/local/bin/wamn-ctl".into()];
    let mut volumes = Vec::new();
    let mut volume_mounts = Vec::new();
    if request.emits_secret {
        command = vec!["/bin/sh".into(), "-ec".into()];
        args.splice(0..0, [HANDOFF.into(), request.name.clone()]);
        volumes.push(Volume {
            name: "out",
            empty_dir: EmptyDir {
                medium: "Memory",
                size_limit: "1Mi",
            },
        });
        volume_mounts.push(VolumeMount {
            name: "out",
            mount_path: "/out",
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
                    containers: vec![Container {
                        name: request.name.clone(),
                        image: request.image.clone(),
                        command,
                        args,
                        env: secrets
                            .iter()
                            .map(|secret| Environment {
                                name: secret.environment.clone(),
                                value_from: SecretSource {
                                    secret_key_ref: SecretKey {
                                        name: secret.name.clone(),
                                        key: secret.key.clone(),
                                    },
                                },
                            })
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
        "inputs": inputs, "image": request.image, "arguments": request.arguments
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
        finish(target, work_directory, &request.name).await?;
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
        for secret in secrets {
            kubectl::apply(
                &target.kubeconfig,
                &target.context,
                &target.namespace,
                &owned_secret(secret, &created.metadata)?,
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
        kubectl::wait(
            &target.kubeconfig,
            &target.context,
            &target.namespace,
            "job",
            &request.name,
            "condition=complete",
            "600s",
        )
        .await
    }
    .await;
    let cleanup = finish(target, work_directory, &request.name).await;
    result?;
    cleanup?;
    kubectl::apply(
        &target.kubeconfig,
        &target.context,
        "platform",
        &Completion {
            api_version: "v1".into(),
            kind: "ConfigMap".into(),
            metadata: metadata(&marker, "platform"),
            data: [("digest".into(), digest)].into(),
        },
    )
    .await?;
    Ok(true)
}

async fn finish(target: &Target, directory: &Path, name: &str) -> anyhow::Result<()> {
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
    // Even a failed Job must release its temporary credentials.
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
    saved?;
    deleted?;
    Ok(())
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
