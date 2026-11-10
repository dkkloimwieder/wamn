//! Fresh Kubernetes Jobs with exact positive and expected refusal verdicts.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::process::Command;

use crate::kubernetes_gate_verdict::PROTOCOL;

/// An executable Job and its exact expected outcome.
#[derive(Debug, Deserialize, Serialize)]
pub struct Job {
    pub name: String,
    pub container: String,
    pub expectation: String,
    pub exit_code: i32,
    pub image: String,
    #[serde(default)]
    pub log_contains: String,
}

/// Exact executable arguments for a before and after side effect snapshot.
#[derive(Debug)]
pub struct Probe {
    pub executable: std::path::PathBuf,
    pub argv: Vec<String>,
    pub environment: Vec<(String, std::ffi::OsString)>,
}

/// Inputs for one aggregate. The caller supplies an argv-only kubectl builder.
#[derive(Debug)]
pub struct Run<'a> {
    pub manifest: &'a Path,
    pub record: &'a Path,
    pub namespace: &'a str,
    pub timeout: Duration,
    pub jobs: &'a [Job],
    pub probe: Option<&'a Probe>,
}

fn hash(bytes: &[u8]) -> String {
    hex::encode(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

// UTC formatting through the system date command preserves RFC3339 nanoseconds.
async fn timestamp() -> anyhow::Result<String> {
    let output = execute(
        Command::new("date").args(["-u", "+%Y-%m-%dT%H:%M:%S.%NZ"]),
        Duration::from_secs(300),
    )
    .await?;
    ensure!(output.status.success(), "UTC timestamp command failed");
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

async fn execute(command: &mut Command, timeout: Duration) -> anyhow::Result<std::process::Output> {
    tokio::time::timeout(timeout, command.kill_on_drop(true).output())
        .await
        .context("Kubernetes gate command deadline")?
        .context("execute Kubernetes gate command")
}

async fn probe(probe: &Probe, timeout: Duration) -> (i32, String) {
    match execute(
        Command::new(&probe.executable)
            .args(&probe.argv)
            .envs(probe.environment.iter().cloned()),
        timeout,
    )
    .await
    {
        Ok(output) => (output.status.code().unwrap_or(255), hash(&output.stdout)),
        Err(_) => (255, hash(&[])),
    }
}

/// Run each named Job afresh and write the versioned verdict even after failures.
pub async fn run(input: &Run<'_>, kubectl: impl Fn() -> Command) -> anyhow::Result<Value> {
    ensure!(
        !input.jobs.is_empty() && input.timeout.as_secs() > 0 && input.timeout.subsec_nanos() == 0,
        "Jobs and a positive deadline are required"
    );
    let mut negatives = 0;
    for job in input.jobs {
        ensure!(
            !job.name.is_empty() && !job.container.is_empty() && !job.image.is_empty(),
            "Job identity must not be empty"
        );
        match job.expectation.as_str() {
            "positive" => ensure!(job.exit_code == 0, "positive Job must exit zero"),
            "expected-negative" => {
                negatives += 1;
                ensure!(
                    job.exit_code > 0 && !job.log_contains.is_empty(),
                    "negative Job requires exact refusal and log"
                );
            }
            _ => anyhow::bail!("invalid Job expectation"),
        }
    }
    ensure!(
        negatives <= 1 && (negatives == 1) == input.probe.is_some(),
        "exactly one negative Job requires a snapshot probe"
    );
    let command = |args: &[&str]| {
        let mut cmd = kubectl();
        cmd.args(["-n", input.namespace]).args(args);
        cmd
    };
    let mut failures = Vec::<String>::new();
    let mut old = Vec::new();
    for job in input.jobs {
        let value = execute(
            &mut command(&["get", "job", &job.name, "-o", "json"]),
            input.timeout,
        )
        .await
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
        .unwrap_or_default();
        old.push(value["metadata"]["uid"].as_str().unwrap_or("").to_owned());
        if !execute(
            &mut command(&[
                "delete",
                "job",
                &job.name,
                "--ignore-not-found=true",
                "--wait=true",
                &format!("--timeout={}s", input.timeout.as_secs()),
            ]),
            input.timeout,
        )
        .await
        .is_ok_and(|o| o.status.success())
        {
            failures.push(format!("delete-failed:{}", job.name));
        }
    }
    let before = if let Some(p) = input.probe {
        Some(probe(p, input.timeout).await)
    } else {
        None
    };
    if before.as_ref().is_some_and(|b| b.0 != 0) {
        failures.push("snapshot-probe-before-failed".into());
    }
    let started = timestamp().await?;
    let mut apply = command(&["apply", "-f"]);
    apply.arg(input.manifest);
    if !execute(&mut apply, input.timeout)
        .await
        .is_ok_and(|o| o.status.success())
    {
        failures.push("apply-failed".into());
    }
    let mut records = Vec::new();
    for (job, previous) in input.jobs.iter().zip(old) {
        let deadline = tokio::time::Instant::now() + input.timeout;
        let want = if job.expectation == "positive" {
            "Complete"
        } else {
            "Failed"
        };
        let mut classes = Vec::<String>::new();
        loop {
            let status = execute(
                &mut command(&["get", "job", &job.name, "-o", "json"]),
                input.timeout,
            )
            .await
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
            .unwrap_or_default();
            if let Some(terminal) = status["status"]["conditions"]
                .as_array()
                .into_iter()
                .flatten()
                .rev()
                .find(|c| {
                    c["status"] == "True" && (c["type"] == "Complete" || c["type"] == "Failed")
                })
            {
                if terminal["type"] != want {
                    classes.push("unexpected-terminal-condition".into());
                }
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                classes.push("job-wait-timeout".into());
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let get = |args: &[&str], missing: &str, fallback: Value| {
            let command = command(args);
            (command, missing.to_owned(), fallback)
        };
        let mut values = Vec::new();
        for (mut cmd, missing, fallback) in [
            get(
                &["get", "job", &job.name, "-o", "json"],
                "job-status-missing",
                json!({}),
            ),
            get(
                &[
                    "get",
                    "pods",
                    "-l",
                    &format!("job-name={}", job.name),
                    "-o",
                    "json",
                ],
                "pod-status-missing",
                json!({"items":[]}),
            ),
        ] {
            match execute(&mut cmd, input.timeout).await {
                Ok(o) if o.status.success() => values.push(serde_json::from_slice(&o.stdout)?),
                _ => {
                    classes.push(missing);
                    values.push(fallback);
                }
            }
        }
        let logs = match execute(
            &mut command(&["logs", &format!("job/{}", job.name), "-c", &job.container]),
            input.timeout,
        )
        .await
        {
            Ok(o) if o.status.success() => {
                if o.stdout.is_empty() {
                    classes.push("logs-empty".into());
                }
                o.stdout
            }
            _ => {
                classes.push("logs-missing".into());
                Vec::new()
            }
        };
        records.push(observe(
            job, &previous, &started, want, &values[0], &values[1], &logs, classes,
        ));
        for class in records.last().unwrap()["failure_classes"]
            .as_array()
            .unwrap()
        {
            failures.push(format!("{}:{}", job.name, class.as_str().unwrap()));
        }
    }
    let snapshot = if let (Some(p), Some(before)) = (input.probe, before) {
        let after = probe(p, input.timeout).await;
        if after.0 != 0 {
            failures.push("snapshot-probe-after-failed".into());
        }
        let unchanged = before.0 == 0 && after.0 == 0 && before.1 == after.1;
        if !unchanged {
            failures.push("negative-side-effect-changed".into());
        }
        json!({"executable":p.executable,"argv":p.argv,"before_exit_code":before.0,"after_exit_code":after.0,"before_stdout_sha256":before.1,"after_stdout_sha256":after.1,"unchanged":unchanged})
    } else {
        Value::Null
    };
    let record = json!({"schema_version":"0.1","protocol":PROTOCOL,"manifest":input.manifest,"namespace":input.namespace,"run_started_at":started,"timeout_seconds":input.timeout.as_secs(),"verdict":if failures.is_empty(){"pass"}else{"fail"},"failure_classes":failures,"jobs":records,"snapshot_probe":snapshot});
    if let Some(parent) = input.record.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(input.record, serde_json::to_vec_pretty(&record)?)?;
    Ok(record)
}

#[expect(
    clippy::too_many_arguments,
    reason = "one observed Job carries its expected and observed inputs"
)]
fn observe(
    job: &Job,
    previous: &str,
    started: &str,
    want: &str,
    status: &Value,
    pods: &Value,
    logs: &[u8],
    mut failures: Vec<String>,
) -> Value {
    let text = |v: &Value| v.as_str().unwrap_or("").to_owned();
    let uid = text(&status["metadata"]["uid"]);
    let created = text(&status["metadata"]["creationTimestamp"]);
    let transition = status["status"]["conditions"]
        .as_array()
        .into_iter()
        .flatten()
        .rfind(|c| c["type"] == want && c["status"] == "True")
        .map(|c| text(&c["lastTransitionTime"]))
        .unwrap_or_default();
    let mut check = |ok: bool, class: &str| {
        if !ok && !failures.iter().any(|failure| failure == class) {
            failures.push(class.into());
        }
    };
    check(
        !uid.is_empty() && (previous.is_empty() || uid != previous),
        "stale-job",
    );
    check(
        !created.is_empty() && created.as_str() >= started,
        "job-predates-run",
    );
    check(
        !transition.is_empty() && transition.as_str() >= started,
        "condition-predates-run",
    );
    let items = pods["items"].as_array().map_or(&[][..], Vec::as_slice);
    check(!items.is_empty(), "pod-never-created");
    let mut observed = Vec::new();
    for pod in items {
        check(
            text(&pod["metadata"]["creationTimestamp"]).as_str() >= started,
            "pod-predates-run",
        );
        check(
            text(&pod["status"]["startTime"]).as_str() >= started,
            "pod-never-started",
        );
        check(
            pod["status"]["phase"]
                == if want == "Complete" {
                    "Succeeded"
                } else {
                    "Failed"
                },
            "pod-phase-mismatch",
        );
        let init = pod["status"]["initContainerStatuses"]
            .as_array()
            .map_or(&[][..], Vec::as_slice);
        let declared = pod["spec"]["initContainers"].as_array().map_or(0, Vec::len);
        check(
            declared == init.len()
                && init.iter().all(|i| {
                    i["state"]["terminated"]["exitCode"] == 0
                        && text(&i["state"]["terminated"]["finishedAt"]).as_str() >= started
                }),
            "init-container-failed",
        );
        check(
            pod["spec"]["containers"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|c| c["name"] == job.container && c["image"] == job.image),
            "wrong-image",
        );
        let container = pod["status"]["containerStatuses"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|c| c["name"] == job.container)
            .cloned()
            .unwrap_or_default();
        check(!text(&container["imageID"]).is_empty(), "image-id-missing");
        check(
            container["state"]["terminated"]["exitCode"].as_i64() == Some(i64::from(job.exit_code)),
            "container-exit-code-mismatch",
        );
        check(
            text(&container["state"]["terminated"]["finishedAt"]).as_str() >= started,
            "container-completion-invalid",
        );
        observed.push(json!({"name":pod["metadata"]["name"].as_str().unwrap_or(""),"uid":pod["metadata"]["uid"].as_str().unwrap_or(""),"node":pod["spec"]["nodeName"].as_str().unwrap_or(""),"created_at":pod["metadata"]["creationTimestamp"].as_str().unwrap_or(""),"started_at":pod["status"]["startTime"],"phase":pod["status"]["phase"].as_str().unwrap_or(""),"init_exit_codes":init.iter().map(|i| i["state"]["terminated"]["exitCode"].clone()).collect::<Vec<_>>(),"container_exit_code":container["state"]["terminated"]["exitCode"],"image_id":container["imageID"].as_str().unwrap_or("")}));
    }
    check(
        job.log_contains.is_empty() || String::from_utf8_lossy(logs).contains(&job.log_contains),
        "expected-log-missing",
    );
    json!({"name":job.name,"container":job.container,"expectation":job.expectation,"expected_exit_code":job.exit_code,"expected_image":job.image,"observed":{"uid":uid,"previous_uid":previous,"created_at":created,"condition":want.to_ascii_lowercase(),"condition_transition_at":transition,"logs_sha256":hash(logs),"pods":observed},"verdict":if failures.is_empty(){"pass"}else{"fail"},"failure_classes":failures})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_observation_refusal_keeps_its_failure_class() {
        let job = Job {
            name: "gate".into(),
            container: "gate".into(),
            expectation: "positive".into(),
            exit_code: 0,
            image: "candidate@sha256:abc".into(),
            log_contains: "PASS".into(),
        };
        let started = "2026-11-03T00:00:00Z";
        let status = json!({"metadata":{"uid":"new","creationTimestamp":started},"status":{"conditions":[{"type":"Complete","status":"True","lastTransitionTime":started}]}});
        let pods = json!({"items":[{"metadata":{"name":"gate-pod","uid":"pod","creationTimestamp":started},"spec":{"nodeName":"node","initContainers":[{"name":"ready"}],"containers":[{"name":"gate","image":job.image}]},"status":{"startTime":started,"phase":"Succeeded","initContainerStatuses":[{"state":{"terminated":{"exitCode":0,"finishedAt":started}}}],"containerStatuses":[{"name":"gate","imageID":"sha256:abc","state":{"terminated":{"exitCode":0,"finishedAt":started}}}]}}]});
        assert_eq!(
            observe(
                &job,
                "old",
                started,
                "Complete",
                &status,
                &pods,
                b"PASS",
                vec![]
            )["verdict"],
            "pass"
        );
        for (is_job, path, value, class) in [
            (true, "/metadata/uid", json!("old"), "stale-job"),
            (
                true,
                "/metadata/creationTimestamp",
                json!("2000-01-01T00:00:00Z"),
                "job-predates-run",
            ),
            (
                true,
                "/status/conditions/0/lastTransitionTime",
                json!("2000-01-01T00:00:00Z"),
                "condition-predates-run",
            ),
            (false, "/items", json!([]), "pod-never-created"),
            (
                false,
                "/items/0/metadata/creationTimestamp",
                json!("2000-01-01T00:00:00Z"),
                "pod-predates-run",
            ),
            (
                false,
                "/items/0/status/startTime",
                Value::Null,
                "pod-never-started",
            ),
            (
                false,
                "/items/0/status/phase",
                json!("Pending"),
                "pod-phase-mismatch",
            ),
            (
                false,
                "/items/0/status/initContainerStatuses",
                json!([]),
                "init-container-failed",
            ),
            (
                false,
                "/items/0/status/initContainerStatuses/0/state/terminated/exitCode",
                json!(1),
                "init-container-failed",
            ),
            (
                false,
                "/items/0/status/initContainerStatuses/0/state/terminated/finishedAt",
                json!("2000-01-01T00:00:00Z"),
                "init-container-failed",
            ),
            (
                false,
                "/items/0/spec/containers/0/image",
                json!("other"),
                "wrong-image",
            ),
            (
                false,
                "/items/0/status/containerStatuses/0/imageID",
                json!(""),
                "image-id-missing",
            ),
            (
                false,
                "/items/0/status/containerStatuses/0/state/terminated/exitCode",
                json!(1),
                "container-exit-code-mismatch",
            ),
            (
                false,
                "/items/0/status/containerStatuses/0/state/terminated/finishedAt",
                json!("2000-01-01T00:00:00Z"),
                "container-completion-invalid",
            ),
        ] {
            let mut changed_status = status.clone();
            let mut changed_pods = pods.clone();
            *(if is_job {
                &mut changed_status
            } else {
                &mut changed_pods
            })
            .pointer_mut(path)
            .unwrap() = value;
            let record = observe(
                &job,
                "old",
                started,
                "Complete",
                &changed_status,
                &changed_pods,
                b"PASS",
                vec![],
            );
            assert_eq!(record["verdict"], "fail", "{class}");
            assert!(
                record["failure_classes"]
                    .as_array()
                    .unwrap()
                    .contains(&json!(class)),
                "{class}: {record}"
            );
        }
        let record = observe(
            &job,
            "old",
            started,
            "Complete",
            &status,
            &pods,
            b"other",
            vec![],
        );
        assert_eq!(record["failure_classes"], json!(["expected-log-missing"]));
    }
}
