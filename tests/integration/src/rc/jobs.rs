//! Run the existing native Jobs and read their Kubernetes completion results.

use std::fs;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde::Deserialize as _;
use serde_json::{Value, json};
use sha2::Digest as _;
use tokio::process::Command;
use tokio::time::Instant;

use super::{
    NAMESPACE, Resources, apply, checked, command_json, kubectl, resources::write_private, save,
};

pub(super) async fn inspect_image(resources: &Resources) -> anyhow::Result<()> {
    let directory = resources.evidence.join("gates-image");
    fs::create_dir(&directory)?;
    let labels = command_json(
        Command::new(&resources.lifecycle).args(["image-labels", &resources.gates_image]),
    )
    .await?;
    write_private(
        &directory.join("host-image-labels.json"),
        &serde_json::to_vec_pretty(&labels)?,
    )?;
    ensure!(
        labels["wamn.dev/source-head"] == resources.source
            && labels["wamn.dev/build-profile"] == "debug",
        "the native test image does not carry the requested source and build profile"
    );
    let nodes = checked(Command::new(&resources.lifecycle).args(["nodes", super::CLUSTER])).await?;
    for node in std::str::from_utf8(&nodes)?.lines() {
        let bytes = checked(Command::new(&resources.lifecycle).args([
            "node-image",
            node,
            &resources.gates_image,
        ]))
        .await?;
        write_private(&directory.join(format!("node-image-{node}.json")), &bytes)?;
    }
    Ok(())
}

pub(super) async fn install_dependencies(resources: &Resources) -> anyhow::Result<()> {
    for (source, name) in [
        ("deploy/infra/otel-collector.yaml", "otel-collector.json"),
        ("deploy/gates/serve-echo.yaml", "serve-echo.json"),
    ] {
        let template = fs::read_to_string(resources.repository.join(source))?;
        let mut documents = Vec::new();
        for document in serde_yaml::Deserializer::from_str(&template) {
            let mut document = Value::deserialize(document)?;
            document["metadata"]["namespace"] = json!(NAMESPACE);
            if document["kind"] == "Deployment" && document["metadata"]["name"] == "serve-echo" {
                let containers = document
                    .pointer_mut("/spec/template/spec/containers")
                    .and_then(Value::as_array_mut)
                    .context("serve-echo has containers")?;
                ensure!(
                    containers.len() == 1 && containers[0]["name"] == "serve-echo",
                    "serve-echo keeps its native container"
                );
                containers[0]["image"] = json!(resources.gates_image);
            }
            documents.push(document);
        }
        let path = resources.work.join(name);
        write_private(
            &path,
            &serde_json::to_vec(&json!({"apiVersion":"v1","kind":"List","items":documents}))?,
        )?;
        apply(resources, &path).await?;
    }
    for name in ["deployment/otel-collector", "deployment/serve-echo"] {
        checked(kubectl(resources).args([
            "-n",
            NAMESPACE,
            "rollout",
            "status",
            name,
            "--timeout=180s",
        ]))
        .await?;
    }
    Ok(())
}

pub(super) async fn run(
    resources: &Resources,
    name: &str,
    timeout: Duration,
) -> anyhow::Result<Value> {
    let template = fs::read_to_string(
        resources
            .repository
            .join(format!("deploy/gates/{name}-job.yaml")),
    )?;
    let mut job: Value = serde_yaml::from_str(&template)?;
    ensure!(
        job["kind"] == "Job" && job["metadata"]["name"] == name,
        "RC selects the declared native Job"
    );
    job["metadata"]["namespace"] = json!(NAMESPACE);
    let pod_spec = job
        .pointer_mut("/spec/template/spec")
        .and_then(Value::as_object_mut)
        .context("Job has a Pod spec")?;
    // The Rust caller has already waited for both dependencies to roll out.
    pod_spec.remove("initContainers");
    let containers = pod_spec
        .get_mut("containers")
        .and_then(Value::as_array_mut)
        .context("Job declares containers")?;
    ensure!(
        containers.len() == 1 && containers[0]["name"] == name,
        "RC retains one native test container"
    );
    containers[0]["image"] = json!(resources.gates_image);
    containers[0]["terminationMessagePath"] = json!("/dev/termination-log");
    containers[0]["terminationMessagePolicy"] = json!("File");
    let args = containers[0]["args"]
        .as_array_mut()
        .context("native test has direct arguments")?;
    if name == "trace-test" {
        let index = args
            .iter()
            .position(|arg| arg == "--upstream")
            .context("trace test declares its upstream")?;
        ensure!(args.get(index + 1).is_some(), "trace upstream has a value");
        args[index + 1] = json!(format!(
            "http://serve-echo.{NAMESPACE}.svc.cluster.local:8091/"
        ));
    }
    args.extend([json!("--result-file"), json!("/dev/termination-log")]);
    let path = resources.work.join(format!("{name}-job.json"));
    write_private(&path, &serde_json::to_vec(&job)?)?;
    // These names belong to this fresh RC namespace. A second run gets a new UID.
    let old = command_json(kubectl(resources).args([
        "-n",
        NAMESPACE,
        "get",
        "job",
        name,
        "--ignore-not-found",
        "-o",
        "json",
    ]))
    .await
    .ok();
    checked(kubectl(resources).args([
        "-n",
        NAMESPACE,
        "delete",
        "job",
        name,
        "--ignore-not-found",
        "--wait=true",
        "--timeout=180s",
    ]))
    .await?;
    let started = chrono::Utc::now().timestamp();
    apply(resources, &path).await?;
    let initial =
        command_json(kubectl(resources).args(["-n", NAMESPACE, "get", "job", name, "-o", "json"]))
            .await?;
    let uid = initial["metadata"]["uid"]
        .as_str()
        .filter(|value| !value.is_empty())
        .context("the new Job has a UID")?;
    ensure!(
        old.as_ref()
            .and_then(|value| value["metadata"]["uid"].as_str())
            != Some(uid),
        "RC must run a fresh Job"
    );
    let deadline = Instant::now() + timeout;
    let terminal = loop {
        let observed = command_json(
            kubectl(resources).args(["-n", NAMESPACE, "get", "job", name, "-o", "json"]),
        )
        .await?;
        save(resources, &format!("{name}-job.json"), &observed)?;
        ensure!(
            observed["metadata"]["uid"] == uid,
            "the running Job was replaced"
        );
        if observed["status"]["conditions"]
            .as_array()
            .is_some_and(|conditions| {
                conditions.iter().any(|condition| {
                    condition["status"] == "True"
                        && (condition["type"] == "Complete" || condition["type"] == "Failed")
                })
            })
        {
            break observed;
        }
        ensure!(
            Instant::now() < deadline,
            "{name} did not finish within its retained deadline"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let pods = command_json(kubectl(resources).args([
        "-n",
        NAMESPACE,
        "get",
        "pods",
        "-l",
        &format!("job-name={name}"),
        "-o",
        "json",
    ]))
    .await?;
    save(resources, &format!("{name}-pods.json"), &pods)?;
    let logs = checked(kubectl(resources).args([
        "-n",
        NAMESPACE,
        "logs",
        &format!("job/{name}"),
        "-c",
        name,
    ]))
    .await?;
    ensure!(!logs.is_empty(), "native test log must not be empty");
    write_private(&resources.evidence.join(format!("{name}.log")), &logs)?;
    validate_times(started, &terminal, &pods)?;
    let result = validate_completion(name, uid, &resources.gates_image, &terminal, &pods)?;
    let verdict = json!({"name":name,"job_uid":uid,"verdict":"pass","failure_classes":[],"logs_sha256":hex::encode(sha2::Sha256::digest(&logs)),"result":result});
    save(resources, &format!("{name}-verdict.json"), &verdict)?;
    Ok(verdict)
}

fn validate_times(started: i64, job: &Value, pods: &Value) -> anyhow::Result<()> {
    let recent = |value: &Value| -> anyhow::Result<()> {
        let value = value
            .as_str()
            .context("native execution has an observed timestamp")?;
        let time = chrono::DateTime::parse_from_rfc3339(value)?;
        ensure!(
            time.timestamp() >= started,
            "native execution timestamp predates this run"
        );
        Ok(())
    };
    recent(&job["metadata"]["creationTimestamp"])?;
    let complete = job["status"]["conditions"]
        .as_array()
        .context("Job has conditions")?
        .iter()
        .find(|condition| condition["type"] == "Complete" && condition["status"] == "True")
        .context("Job completed")?;
    recent(&complete["lastTransitionTime"])?;
    for pod in pods["items"].as_array().context("Job has Pods")? {
        recent(&pod["metadata"]["creationTimestamp"])?;
        recent(&pod["status"]["startTime"])?;
        let declared = pod["spec"]["initContainers"].as_array().map_or(0, Vec::len);
        let observed = pod["status"]["initContainerStatuses"].as_array();
        ensure!(
            declared == observed.map_or(0, Vec::len),
            "every declared initializer must finish"
        );
        if let Some(statuses) = observed {
            for status in statuses {
                ensure!(
                    status["state"]["terminated"]["exitCode"] == 0,
                    "initializer must exit successfully"
                );
                recent(&status["state"]["terminated"]["finishedAt"])?;
            }
        }
        for status in pod["status"]["containerStatuses"]
            .as_array()
            .context("Pod has native container statuses")?
        {
            recent(&status["state"]["terminated"]["finishedAt"])?;
        }
    }
    Ok(())
}

fn validate_completion(
    name: &str,
    uid: &str,
    image: &str,
    job: &Value,
    pods: &Value,
) -> anyhow::Result<Value> {
    ensure!(
        job["metadata"]["uid"] == uid
            && job["status"]["conditions"]
                .as_array()
                .is_some_and(|conditions| conditions
                    .iter()
                    .any(|condition| condition["type"] == "Complete"
                        && condition["status"] == "True")),
        "native Job must complete under its original UID"
    );
    ensure!(
        job["status"]["failed"].as_u64().unwrap_or(0) == 0,
        "native Job must not fail an attempt"
    );
    let items = pods["items"]
        .as_array()
        .context("Job Pods are a Kubernetes list")?;
    ensure!(items.len() == 1, "native Job must have exactly one Pod");
    let pod = &items[0];
    ensure!(
        pod["metadata"]["uid"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
            && pod["metadata"]["namespace"] == NAMESPACE
            && pod["status"]["phase"] == "Succeeded",
        "native Pod must succeed in the RC namespace"
    );
    ensure!(
        pod["metadata"]["ownerReferences"]
            .as_array()
            .is_some_and(|owners| owners.iter().any(|owner| owner["uid"] == uid
                && owner["kind"] == "Job"
                && owner["controller"] == true)),
        "native Pod must belong to the fresh Job"
    );
    let containers = pod["spec"]["containers"]
        .as_array()
        .context("native Pod has containers")?;
    ensure!(
        containers.len() == 1 && containers[0]["name"] == name && containers[0]["image"] == image,
        "native Pod must use the selected test image"
    );
    let statuses = pod["status"]["containerStatuses"]
        .as_array()
        .context("native Pod has container statuses")?;
    ensure!(
        statuses.len() == 1
            && statuses[0]["name"] == name
            && statuses[0]["restartCount"] == 0
            && statuses[0]["imageID"]
                .as_str()
                .is_some_and(|value| !value.is_empty()),
        "native test must run once with an observed image ID"
    );
    let terminated = &statuses[0]["state"]["terminated"];
    ensure!(
        terminated["exitCode"] == 0,
        "native test must exit successfully"
    );
    let result: Value = serde_json::from_str(
        terminated["message"]
            .as_str()
            .context("native test returns its assertions through the termination file")?,
    )?;
    ensure!(
        result["test"] == name && result["passed"] == true,
        "native test must report its own successful assertions"
    );
    if name == "socket-test" {
        ensure!(
            result["checks"]["P2"]["refused"] == true
                && result["checks"]["P3"]["refused"] == true
                && result["checks"]["standard"]["admitted"] == true,
            "both socket importers must be refused and the standard workload admitted"
        );
    } else {
        ensure!(
            result["surface"] == "wamn:connection/http"
                && result["injected"].is_string()
                && result["injected"] == result["reflected"]["traceparent"],
            "trace result must retain the actual host-injected and reflected header"
        );
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn completed() -> (Value, Value) {
        let result = json!({"test":"socket-test","passed":true,"checks":{"P2":{"refused":true},"P3":{"refused":true},"standard":{"admitted":true}}});
        (
            json!({"metadata":{"uid":"new-job"},"status":{"conditions":[{"type":"Complete","status":"True"}]}}),
            json!({"items":[{"metadata":{"uid":"new-pod","namespace":NAMESPACE,"ownerReferences":[{"uid":"new-job","kind":"Job","controller":true}]},"spec":{"containers":[{"name":"socket-test","image":"test:image"}]},"status":{"phase":"Succeeded","containerStatuses":[{"name":"socket-test","restartCount":0,"imageID":"test@sha256:abc","state":{"terminated":{"exitCode":0,"message":result.to_string()}}}]}}]}),
        )
    }
    #[test]
    fn observed_times_must_belong_to_this_execution() {
        let (mut job, mut pods) = completed();
        let now = "2026-09-11T12:00:00Z";
        job["metadata"]["creationTimestamp"] = json!(now);
        job["status"]["conditions"][0]["lastTransitionTime"] = json!(now);
        pods["items"][0]["metadata"]["creationTimestamp"] = json!(now);
        pods["items"][0]["status"]["startTime"] = json!(now);
        pods["items"][0]["status"]["containerStatuses"][0]["state"]["terminated"]["finishedAt"] =
            json!(now);
        let started = chrono::DateTime::parse_from_rfc3339(now)
            .unwrap()
            .timestamp();
        validate_times(started, &job, &pods).unwrap();
        for path in [
            "/items/0/metadata/creationTimestamp",
            "/items/0/status/startTime",
            "/items/0/status/containerStatuses/0/state/terminated/finishedAt",
        ] {
            let mut changed = pods.clone();
            *changed.pointer_mut(path).unwrap() = json!("2026-09-11T11:59:59Z");
            assert!(validate_times(started, &job, &changed).is_err());
        }
    }

    #[test]
    fn native_completion_accepts_the_recorded_config_image_id() {
        // This historical Pod ran the gates image without a repository digest.
        let observed: Value = serde_json::from_str(include_str!(
            "../../../../docs/perf/2026.09/ctc8-15-1-identity/deployed-001/identity-jwks-published-a-pods.json"
        ))
        .unwrap();
        let image: Value = serde_json::from_str(include_str!(
            "../../../../docs/perf/2026.09/ctc8-15-1-identity/deployed-001/gates-node-image.json"
        ))
        .unwrap();
        let image_id = &observed["items"][0]["status"]["containerStatuses"][0]["imageID"];
        assert_eq!(image["status"]["repoDigests"], json!([]));
        assert_eq!(image_id, &image["status"]["id"]);
        let (job, mut pods) = completed();
        pods["items"][0]["status"]["containerStatuses"][0]["imageID"] = image_id.clone();
        assert!(validate_completion("socket-test", "new-job", "test:image", &job, &pods).is_ok());
    }

    #[test]
    fn completion_requires_original_job_image_and_assertions() {
        let (job, pods) = completed();
        assert!(validate_completion("socket-test", "new-job", "test:image", &job, &pods).is_ok());
        for (path, value) in [
            ("/items/0/metadata/ownerReferences/0/uid", json!("old-job")),
            ("/items/0/spec/containers/0/image", json!("other:image")),
            ("/items/0/status/containerStatuses/0/imageID", Value::Null),
            ("/items/0/status/containerStatuses/0/imageID", json!("")),
            (
                "/items/0/status/containerStatuses/0/state/terminated/exitCode",
                json!(1),
            ),
            (
                "/items/0/status/containerStatuses/0/state/terminated/message",
                json!("{}"),
            ),
        ] {
            let mut changed = pods.clone();
            *changed.pointer_mut(path).unwrap() = value;
            assert!(
                validate_completion("socket-test", "new-job", "test:image", &job, &changed)
                    .is_err()
            );
        }
    }
}
