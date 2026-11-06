//! Kubernetes lifecycle for the platform's initialization Jobs.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::kubectl;
use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_release_control::release_chart::Target;

const OUTPUT_FILE: &str = "/out/secret.json";
// Bound execution even if the caller exits. Kubernetes never retries these Jobs.
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
const READ_OUTPUT: &str = "if [ -f /out/secret.json ]; then cat /out/secret.json; fi";
const ACK_OUTPUT: &str = "rm -f /out/secret.json";

#[derive(Debug)]
struct JobError {
    name: String,
}
impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "initialization Job {} failed", self.name)
    }
}
impl std::error::Error for JobError {}

/// A temporary Kubernetes Secret. Debug output excludes its values.
pub(crate) struct Secret(pub(crate) Value);
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Secret")
            .field("name", &self.0["metadata"]["name"])
            .finish_non_exhaustive()
    }
}

pub(crate) fn manifest(name: &str, image: &str, command: &[&str], arguments: &[&str]) -> Value {
    json!({"apiVersion":"batch/v1", "kind":"Job", "metadata":{"name":name},
        "spec":{"suspend":true, "backoffLimit":0, "activeDeadlineSeconds":INSTALL_DEADLINE.as_secs(),
            "template":{"spec":{"restartPolicy":"Never", "automountServiceAccountToken":false,
                "containers":[{"name":name, "image":image, "command":command, "args":arguments, "env":[], "volumeMounts":[]}],
                "volumes":[]}}}})
}

pub(crate) fn name(job: &Value) -> &str {
    job["metadata"]["name"]
        .as_str()
        .expect("initialization Job name")
}

pub(crate) fn container(job: &mut Value) -> &mut Value {
    &mut job["spec"]["template"]["spec"]["containers"][0]
}

pub(crate) fn environment(job: &mut Value, values: &[(&str, &str)]) {
    let env = container(job)["env"].as_array_mut().unwrap();
    env.extend(
        values
            .iter()
            .map(|(name, value)| json!({"name":name,"value":value})),
    );
}

pub(crate) fn credential(
    job: &mut Value,
    name: &str,
    environment: &str,
    key: &str,
    value: &str,
) -> Secret {
    container(job)["env"].as_array_mut().unwrap().push(json!({
        "name":environment, "valueFrom":{"secretKeyRef":{"name":name,"key":key}}
    }));
    Secret(
        json!({"apiVersion":"v1", "kind":"Secret", "metadata":{"name":name},
        "type":"Opaque", "stringData":{key:value}}),
    )
}

pub(crate) fn mount(job: &mut Value, name: &str, secret: &str, path: &str, keys: &[&str]) {
    job["spec"]["template"]["spec"]["volumes"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "name":name, "secret":{"secretName":secret, "defaultMode":256,
                "items":keys.iter().map(|key| json!({"key":key,"path":key})).collect::<Vec<_>>()}
        }));
    container(job)["volumeMounts"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":name, "mountPath":path, "readOnly":true}));
}

pub(crate) fn files(job: &mut Value, files: BTreeMap<String, String>) -> Secret {
    let secret = format!("{}-files", name(job));
    mount(
        job,
        "inputs",
        &secret,
        "/inputs",
        &files.keys().map(String::as_str).collect::<Vec<_>>(),
    );
    let mut object =
        json!({"apiVersion":"v1", "kind":"Secret", "metadata":{"name":secret}, "type":"Opaque"});
    object["stringData"] = Value::Object(
        files
            .into_iter()
            .map(|(key, value)| (key, Value::String(value)))
            .collect(),
    );
    Secret(object)
}

pub(crate) fn secret_output(job: &mut Value) {
    let name = name(job).to_owned();
    let container = container(job);
    container["command"] = json!(["/bin/sh", "-ec"]);
    container["args"]
        .as_array_mut()
        .unwrap()
        .splice(0..0, [HANDOFF.into(), name.into()]);
    container["volumeMounts"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"out", "mountPath":"/out"}));
    job["spec"]["template"]["spec"]["volumes"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "name":"out", "emptyDir":{"medium":"Memory", "sizeLimit":"1Mi"}
        }));
}

fn emits_secret(job: &Value) -> bool {
    job["spec"]["template"]["spec"]["containers"][0]["args"][0] == HANDOFF
}

fn owner_reference(metadata: &Value) -> anyhow::Result<Value> {
    let uid = metadata["uid"].as_str().context("created Job has no uid")?;
    Ok(json!([{"apiVersion":"batch/v1", "kind":"Job", "name":metadata["name"], "uid":uid}]))
}

/// Run a Job and return its public output after cleanup.
///
/// Existing Jobs are collected before a new request runs. Interrupted handoffs
/// remain available for recovery until their Kubernetes deadline expires.
///
/// # Errors
/// Returns execution, handoff, log, or cleanup failures.
pub(crate) async fn run(
    target: &Target,
    directory: &Path,
    job: &mut Value,
    secrets: &mut [Secret],
) -> anyhow::Result<String> {
    let existing: Option<Value> = kubectl::get(
        &target.kubeconfig,
        &target.context,
        &target.namespace,
        "job",
        name(job),
    )
    .await?;
    if let Some(existing) = existing {
        // A suspended Job never started. Its support objects can be incomplete.
        if existing["spec"]["suspend"] == true {
            cleanup(target, &existing).await?;
        } else {
            collect(target, directory, &existing).await?;
        }
    }
    job["metadata"]["namespace"] = target.namespace.clone().into();
    let input = serde_json::to_vec(job)?;
    let created = kubectl::kubectl(
        &target.kubeconfig,
        &target.context,
        &target.namespace,
        &["create", "-f", "-", "-o", "json"],
        Some(&input),
    )
    .await?;
    let created: Value = serde_json::from_str(&created).context("decode the created Job")?;
    let prepared = async {
        for secret in secrets {
            let object = &mut secret.0;
            object["metadata"]["namespace"] = target.namespace.clone().into();
            object["metadata"]["ownerReferences"] = owner_reference(&created["metadata"])?;
            kubectl::apply(
                &target.kubeconfig,
                &target.context,
                &target.namespace,
                object,
            )
            .await?;
        }
        anyhow::Ok(())
    }
    .await;
    if let Err(error) = prepared {
        return match cleanup(target, job).await {
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
            name(job),
            "--type=merge",
            "--patch",
            r#"{"spec":{"suspend":false}}"#,
        ],
        None,
    )
    .await?;
    collect(target, directory, job).await
}

fn completed(status: &Value, name: &str) -> Result<bool, JobError> {
    let Some(conditions) = status["status"]["conditions"].as_array() else {
        return Ok(false);
    };
    if conditions
        .iter()
        .any(|condition| condition["type"] == "Failed" && condition["status"] == "True")
    {
        return Err(JobError { name: name.into() });
    }
    Ok(conditions
        .iter()
        .any(|condition| condition["type"] == "Complete" && condition["status"] == "True"))
}

async fn wait(target: &Target, job: &Value) -> anyhow::Result<()> {
    let deadline = Instant::now() + INSTALL_DEADLINE;
    let mut handed_off = false;
    loop {
        let status: Value = match kubectl::get(
            &target.kubeconfig,
            &target.context,
            &target.namespace,
            "job",
            name(job),
        )
        .await
        {
            Err(error) if error.is::<kubectl::Unreachable>() && Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
            status => status?.context("bootstrap Job disappeared")?,
        };
        if completed(&status, name(job))? {
            return Ok(());
        }
        if emits_secret(job) && !handed_off && status["status"]["ready"].as_u64().unwrap_or(0) > 0 {
            handed_off = handoff(target, job).await?;
        }
        ensure!(
            Instant::now() < deadline,
            "bootstrap Job {} did not complete in {}s",
            name(job),
            INSTALL_DEADLINE.as_secs()
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn handoff(target: &Target, job: &Value) -> anyhow::Result<bool> {
    let job = format!("job/{}", name(job));
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
        secret["metadata"]["namespace"]
            .as_str()
            .context("emitted Secret namespace")?,
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

async fn collect(target: &Target, directory: &Path, job: &Value) -> anyhow::Result<String> {
    let result = wait(target, job).await;
    let output = logs(target, directory, job).await;
    // An API or handoff failure says nothing about execution. Preserve the Job
    // and its owned credentials so the next invocation can recover its result.
    let cleanup = if result.is_ok() || result.as_ref().is_err_and(anyhow::Error::is::<JobError>) {
        cleanup(target, job).await
    } else {
        Ok(())
    };
    result.with_context(|| {
        let path = directory.join("jobs").join(format!("{}.log", name(job)));
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
    output
}

fn emitted_secret(output: &str) -> anyhow::Result<Value> {
    // Parser diagnostics can quote a credential. Keep them out of errors.
    let secret: Value = serde_json::from_str(output)
        .map_err(|_| anyhow::anyhow!("decode the emitted Secret from {OUTPUT_FILE}"))?;
    ensure!(
        secret["apiVersion"] == "v1" && secret["kind"] == "Secret",
        "the Job output is not a v1 Secret"
    );
    Ok(secret)
}

async fn logs(target: &Target, directory: &Path, job: &Value) -> anyhow::Result<String> {
    let name = name(job);
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

async fn cleanup(target: &Target, job: &Value) -> anyhow::Result<()> {
    let name = name(job);
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_uses_the_runner_and_only_its_owned_secret() {
        let mut job = manifest(
            "platform-system",
            "registry/runner@sha256:abc",
            &["/usr/local/bin/wamn-ctl"],
            &["provision-system", "--platform-domain", "example.com"],
        );
        let secret = credential(
            &mut job,
            "wamn-system-admin",
            "WAMN_SYSTEM_ADMIN_URL",
            "url",
            "private",
        );
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
            pod["containers"][0]["env"][0]["valueFrom"]["secretKeyRef"],
            json!({"name":"wamn-system-admin", "key":"url"})
        );
        assert!(!job.to_string().contains("private"));
        assert!(!format!("{secret:?}").contains("private"));
    }

    #[test]
    fn secret_ownership_requires_the_created_jobs_uid() {
        let owner = json!({"name":"platform-system", "namespace":"platform", "uid":"live-uid"});
        assert_eq!(
            owner_reference(&owner).unwrap(),
            json!([{
                "apiVersion":"batch/v1", "kind":"Job", "name":"platform-system", "uid":"live-uid"
            }])
        );
        assert!(owner_reference(&json!({"name":"platform-system"})).is_err());
    }

    #[test]
    fn emitted_secret_uses_memory_and_a_bounded_handoff() {
        let mut job = manifest(
            "platform-identity-issuer",
            "ctl-image",
            &["/usr/local/bin/wamn-ctl"],
            &["provision-identity-issuer"],
        );
        secret_output(&mut job);
        let pod = &job["spec"]["template"]["spec"];
        assert_eq!(
            pod["volumes"],
            json!([{"name":"out", "emptyDir":{"medium":"Memory", "sizeLimit":"1Mi"}}])
        );
        assert_eq!(pod["containers"][0]["args"][0], HANDOFF);
        assert_eq!(pod["containers"][0]["args"][1], "platform-identity-issuer");
        assert_eq!(pod["containers"][0]["args"][2], "provision-identity-issuer");
        assert!(emits_secret(&job));
    }

    #[test]
    fn mounted_credentials_never_enter_the_job_or_debug_output() {
        let mut job = manifest(
            "platform-invite-acme",
            "ctl-image",
            &["/usr/local/bin/wamn-ctl"],
            &["invite"],
        );
        let secret = files(
            &mut job,
            [
                ("ca.crt".into(), "private-mounted-ca".into()),
                ("tls.crt".into(), "private-mounted-cert".into()),
                ("tls.key".into(), "private-mounted-key".into()),
            ]
            .into(),
        );
        assert_eq!(secret.0["stringData"]["tls.key"], "private-mounted-key");
        assert!(!job.to_string().contains("private-mounted-key"));
        assert!(!format!("{secret:?}").contains("private-mounted-key"));
        assert_eq!(
            job["spec"]["template"]["spec"]["volumes"][0]["secret"],
            json!({"secretName":"platform-invite-acme-files", "defaultMode":256,
                "items":[{"key":"ca.crt", "path":"ca.crt"}, {"key":"tls.crt", "path":"tls.crt"}, {"key":"tls.key", "path":"tls.key"}]})
        );
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
        assert_eq!(error.name, "platform-system");
        assert!(!completed(&json!({"status":{"active":1}}), "platform-system").unwrap());
        let mut status = json!({"status":{"conditions":[{"type":"Complete","status":"False"}]}});
        assert!(!completed(&status, "platform-system").unwrap());
        status["status"]["conditions"][0]["status"] = "True".into();
        assert!(completed(&status, "platform-system").unwrap());
    }
}
