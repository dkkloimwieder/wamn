//! Cold, restarted, and steady WMS requests retain their measured limits.

use std::fs;
use std::path::Path;

use crate::startup_support::{
    assert_host_restart, assert_response, request_job, restart_trace_is_complete,
};
use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_gate_harness::case_release::NAMESPACE;
use wamn_gate_harness::journey::JourneyDocument;
use wamn_test_infrastructure::telemetry::SAMPLED_TRACE_IDS;

use super::deployment::{checked, kubectl};
use crate::wms_runtime_live::write_result;

#[derive(Debug)]
pub(super) struct ColdHost {
    pod: String,
    /// The HTTP role Service of the environment release.
    service: String,
    uid: String,
    startup_ms: u64,
}

/// The one host of the environment release `group`, which the release
/// installed with one replica and no request has reached.
pub(super) async fn cold_host(
    cluster: &str,
    work: &Path,
    group: &str,
    evidence: &Path,
) -> anyhow::Result<ColdHost> {
    let host_deployment = format!("hostgroup-{group}");
    checked(kubectl(cluster, work).args([
        "-n",
        NAMESPACE,
        "wait",
        "--for=condition=Available",
        &format!("deployment/{host_deployment}"),
        "--timeout=120s",
    ]))
    .await?;
    let deployment = object(cluster, work, "deployment", &host_deployment).await?;
    write_result(evidence, "host-measurement-deployment.json", &deployment)?;
    assert_native_probes(&deployment)?;
    let selector = deployment["spec"]["selector"]["matchLabels"]
        .as_object()
        .context("host selector has labels")?;
    ensure!(!selector.is_empty(), "host selector is empty");
    let selector = selector
        .iter()
        .map(|(name, value)| {
            Ok(format!(
                "{name}={}",
                value.as_str().context("host selector label is text")?
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?
        .join(",");
    let pods: Value = serde_json::from_slice(
        &checked(kubectl(cluster, work).args([
            "-n", NAMESPACE, "get", "pods", "-l", &selector, "-o", "json",
        ]))
        .await?,
    )?;
    write_result(evidence, "host-cold.json", &pods)?;
    let pods = pods["items"].as_array().context("host response has pods")?;
    ensure!(
        pods.len() == 1,
        "startup measurement requires exactly one host pod"
    );
    assert_host_restart(&pods[0], None, 0)?;
    let pod = pods[0]["metadata"]["name"]
        .as_str()
        .context("cold host has a pod name")?
        .to_owned();
    let uid = pods[0]["metadata"]["uid"]
        .as_str()
        .context("cold host has a UID")?
        .to_owned();
    let startup_ms = startup_time(cluster, work, &pod, evidence, "host-cold.log").await?;
    Ok(ColdHost {
        pod,
        service: format!("{group}-http"),
        uid,
        startup_ms,
    })
}

pub(super) async fn requests(
    cluster: &str,
    work: &Path,
    inputs: &JourneyDocument,
    cold: &ColdHost,
    evidence: &Path,
) -> anyhow::Result<()> {
    // Sampled parent contexts keep these IDs through the host sampler.
    request(
        cluster,
        work,
        &cold.service,
        inputs,
        &RequestTrace {
            name: "cold",
            trace_id: SAMPLED_TRACE_IDS[0],
            parent_span: "1111111111111111",
        },
        evidence,
    )
    .await?;
    let cache = cache_files(cluster, work, &cold.pod, evidence, "cold").await?;
    ensure!(
        !cache.0.is_empty(),
        "cold first request persisted no compiled artifact"
    );
    checked(kubectl(cluster, work).args([
        "-n",
        NAMESPACE,
        "exec",
        &cold.pod,
        "-c",
        "host",
        "--",
        "/bin/sh",
        "-c",
        "kill -TERM 1; exit 0",
    ]))
    .await?;
    checked(kubectl(cluster, work).args([
        "-n",
        NAMESPACE,
        "wait",
        "--for=jsonpath={.status.containerStatuses[0].restartCount}=1",
        &format!("pod/{}", cold.pod),
        "--timeout=120s",
    ]))
    .await?;
    checked(kubectl(cluster, work).args([
        "-n",
        NAMESPACE,
        "wait",
        "--for=condition=Ready",
        &format!("pod/{}", cold.pod),
        "--timeout=120s",
    ]))
    .await?;
    let warm = object(cluster, work, "pod", &cold.pod).await?;
    write_result(evidence, "host-warm.json", &warm)?;
    assert_host_restart(&warm, Some(&cold.uid), 1)?;
    let restarted_ms = startup_time(cluster, work, &cold.pod, evidence, "host-warm.log").await?;
    fs::write(
        evidence.join("host-cold-previous.log"),
        checked(kubectl(cluster, work).args([
            "-n",
            NAMESPACE,
            "logs",
            "--previous",
            "--timestamps",
            &cold.pod,
            "-c",
            "host",
        ]))
        .await?,
    )?;
    route_after_restart(cluster, work, &cold.service, evidence).await?;
    let restart = request(
        cluster,
        work,
        &cold.service,
        inputs,
        &RequestTrace {
            name: "restart-first",
            trace_id: SAMPLED_TRACE_IDS[1],
            parent_span: "2222222222222222",
        },
        evidence,
    )
    .await?;
    let steady = request(
        cluster,
        work,
        &cold.service,
        inputs,
        &RequestTrace {
            name: "steady",
            trace_id: SAMPLED_TRACE_IDS[2],
            parent_span: "3333333333333333",
        },
        evidence,
    )
    .await?;
    let warm_cache = cache_files(cluster, work, &cold.pod, evidence, "warm").await?;
    ensure!(
        cache == warm_cache,
        "the restarted host changed compiled cache bytes, inodes, times, sizes, or paths"
    );
    trace(
        cluster,
        work,
        "restart-first",
        SAMPLED_TRACE_IDS[1],
        restart,
        evidence,
    )
    .await?;
    trace(
        cluster,
        work,
        "steady",
        SAMPLED_TRACE_IDS[2],
        steady,
        evidence,
    )
    .await?;
    write_result(
        evidence,
        "runtime-startup.json",
        &json!({"passed":true,"pod_uid":cold.uid,"cold_startup_ms":cold.startup_ms,"restart_startup_ms":restarted_ms,"cache_entries":warm_cache.0.len()}),
    )
}

/// The trace identity one measured request is issued under.
struct RequestTrace<'a> {
    name: &'a str,
    trace_id: &'a str,
    parent_span: &'a str,
}

async fn request(
    cluster: &str,
    work: &Path,
    service: &str,
    inputs: &JourneyDocument,
    trace: &RequestTrace<'_>,
    evidence: &Path,
) -> anyhow::Result<f64> {
    let RequestTrace {
        name,
        trace_id,
        parent_span,
    } = *trace;
    let caller: Value = serde_json::from_slice(&fs::read(&inputs.operator_secret_output)?)?;
    let secret = caller["metadata"]["name"]
        .as_str()
        .context("operator Secret has a name")?;
    // A get is a read: its one request element travels in the query string, with no
    // request identity.
    let query = wamn_execution_contract::encode_read_query(
        json!({"id":super::application::PACKAGING_ID})
            .as_object()
            .context("the get item is an object")?,
    );
    let job = format!("startup-request-{name}");
    let manifest = request_job(
        NAMESPACE,
        service,
        &inputs.route_host,
        secret,
        &job,
        (trace_id, parent_span),
        &query,
    );
    let path = work.join(format!("{job}.json"));
    fs::write(&path, serde_json::to_vec_pretty(&manifest)?)?;
    checked(kubectl(cluster, work).args(["apply", "-f"]).arg(&path)).await?;
    let completed = checked(kubectl(cluster, work).args([
        "-n",
        NAMESPACE,
        "wait",
        "--for=condition=Complete",
        &format!("job/{job}"),
        "--timeout=240s",
    ]))
    .await;
    let pods: Value = serde_json::from_slice(
        &checked(kubectl(cluster, work).args([
            "-n",
            NAMESPACE,
            "get",
            "pods",
            "-l",
            &format!("job-name={job}"),
            "-o",
            "json",
        ]))
        .await?,
    )?;
    write_result(evidence, &format!("first-request-{name}-pods.json"), &pods)?;
    let items = pods["items"]
        .as_array()
        .context("request Job lists its pods")?;
    ensure!(items.len() == 1, "request Job requires exactly one pod");
    let statuses = items[0]["status"]["containerStatuses"]
        .as_array()
        .context("request pod has container status")?;
    let status = statuses
        .iter()
        .find(|status| status["name"] == "probe")
        .context("request probe has container status")?;
    let message = status["state"]["terminated"]["message"]
        .as_str()
        .context("request probe returned a structured termination result")?;
    let result: Value = serde_json::from_str(message)?;
    write_result(evidence, &format!("first-request-{name}.json"), &result)?;
    // Attempt logs are retained as evidence. Assertions read the structured pod result.
    fs::write(
        evidence.join(format!("first-request-{name}-attempts.jsonl")),
        checked(kubectl(cluster, work).args(["-n", NAMESPACE, "logs", &format!("job/{job}")]))
            .await?,
    )?;
    completed?;
    ensure!(
        status["state"]["terminated"]["exitCode"] == 0,
        "request probe failed"
    );
    let total_ms = assert_response(&result, name == "restart-first")?;
    let response: Value = serde_json::from_slice(&hex::decode(
        result["body_hex"]
            .as_str()
            .context("request response has bytes")?,
    )?)?;
    write_result(
        evidence,
        &format!("first-request-{name}-response.json"),
        &response,
    )?;
    fs::write(
        evidence.join(format!("first-request-{name}.trace-id")),
        format!("{trace_id}\n"),
    )?;
    Ok(total_ms)
}

async fn route_after_restart(
    cluster: &str,
    work: &Path,
    service: &str,
    evidence: &Path,
) -> anyhow::Result<()> {
    let previous: Value =
        serde_json::from_slice(&fs::read(evidence.join("http-replicaset.json"))?)?;
    let uid = previous["metadata"]["uid"]
        .as_str()
        .context("HTTP ReplicaSet has a UID")?;
    for _ in 0..120 {
        let workloads: Value = serde_json::from_slice(
            &checked(kubectl(cluster, work).args([
                "-n",
                NAMESPACE,
                "get",
                "workloads",
                "-o",
                "json",
            ]))
            .await?,
        )?;
        let workloads = workloads["items"]
            .as_array()
            .context("native workloads have items")?
            .iter()
            .filter(|workload| {
                workload["metadata"]["ownerReferences"]
                    .as_array()
                    .is_some_and(|owners| owners.iter().any(|owner| owner["uid"] == uid))
            })
            .collect::<Vec<_>>();
        let slices: Value = serde_json::from_slice(
            &checked(kubectl(cluster, work).args([
                "-n",
                NAMESPACE,
                "get",
                "endpointslices",
                "-l",
                &format!("kubernetes.io/service-name={service},wasmcloud.dev/route-manager=true"),
                "-o",
                "json",
            ]))
            .await?,
        )?;
        write_result(evidence, "flow-http-endpointslices-warm.json", &slices)?;
        if workloads.len() == 1 {
            write_result(evidence, "flow-http-workload-warm.json", workloads[0])?;
            let ready = workloads[0]["status"]["conditions"]
                .as_array()
                .is_some_and(|conditions| {
                    conditions.iter().any(|condition| {
                        condition["type"] == "Ready" && condition["status"] == "True"
                    })
                });
            let slices = slices["items"]
                .as_array()
                .context("route endpoint list has items")?;
            if ready
                && slices.len() == 1
                && slices[0]["endpoints"].as_array().is_some_and(|endpoints| {
                    endpoints.len() == 1
                        && endpoints[0]["conditions"]["ready"] == true
                        && endpoints[0]["conditions"]["serving"] == true
                })
            {
                return Ok(());
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    anyhow::bail!("the restarted HTTP workload and its route endpoint did not become ready")
}

async fn cache_files(
    cluster: &str,
    work: &Path,
    pod: &str,
    evidence: &Path,
    name: &str,
) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    let mut metadata = String::from_utf8(
        checked(kubectl(cluster, work).args([
            "-n",
            NAMESPACE,
            "exec",
            pod,
            "-c",
            "host",
            "--",
            "find",
            "/tmp/wamn-wasmtime-cache/modules",
            "-type",
            "f",
            "!",
            "-name",
            "*.stats",
            "-printf",
            "%i %T@ %s %p\n",
        ]))
        .await?,
    )?
    .lines()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let mut hashes = String::from_utf8(
        checked(kubectl(cluster, work).args([
            "-n",
            NAMESPACE,
            "exec",
            pod,
            "-c",
            "host",
            "--",
            "find",
            "/tmp/wamn-wasmtime-cache/modules",
            "-type",
            "f",
            "!",
            "-name",
            "*.stats",
            "-exec",
            "sha256sum",
            "{}",
            "+",
        ]))
        .await?,
    )?
    .lines()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    metadata.sort();
    hashes.sort();
    fs::write(
        evidence.join(format!("cache-{name}.txt")),
        metadata.join("\n") + "\n",
    )?;
    fs::write(
        evidence.join(format!("cache-{name}.sha256")),
        hashes.join("\n") + "\n",
    )?;
    Ok((metadata, hashes))
}

async fn trace(
    cluster: &str,
    work: &Path,
    name: &str,
    id: &str,
    total_ms: f64,
    evidence: &Path,
) -> anyhow::Result<()> {
    let value = wamn_test_infrastructure::telemetry::read_trace(
        cluster,
        work,
        id,
        evidence,
        restart_trace_is_complete,
    )
    .await?;
    let bytes = serde_json::to_vec_pretty(&value)?;
    fs::write(evidence.join(format!("trace-{name}.json")), &bytes)?;
    ensure!(
        restart_trace_is_complete(&value),
        "{name} trace must have one WMS invocation and statement without component loading or compilation"
    );
    let breakdown = trace_breakdown(&value, name, total_ms)?;
    write_result(
        evidence,
        &format!("trace-breakdown-{name}.json"),
        &breakdown,
    )
}

fn trace_breakdown(document: &Value, name: &str, total_ms: f64) -> anyhow::Result<Value> {
    let mut spans = Vec::new();
    for batch in document["resourceSpans"]
        .as_array()
        .context("trace has resource spans")?
    {
        for scope in batch["scopeSpans"]
            .as_array()
            .context("trace has span scopes")?
        {
            spans.extend(scope["spans"].as_array().context("trace has spans")?);
        }
    }
    let sum = |name: &str, authority: Option<&str>| -> anyhow::Result<f64> {
        let mut total = 0.0;
        for span in &spans {
            if span["name"] != name
                || authority.is_some_and(|authority| {
                    !span["attributes"].as_array().is_some_and(|attributes| {
                        attributes.iter().any(|attribute| {
                            attribute["key"] == "wamn.authority_class"
                                && attribute["value"]["stringValue"] == authority
                        })
                    })
                })
            {
                continue;
            }
            let nanos = |field: &str| -> anyhow::Result<u64> {
                match &span[field] {
                    Value::String(value) => Ok(value.parse()?),
                    Value::Number(value) => value
                        .as_u64()
                        .context("span timestamp is a nonnegative integer"),
                    _ => anyhow::bail!("span timestamp is absent"),
                }
            };
            let start = nanos("startTimeUnixNano")?;
            let end = nanos("endTimeUnixNano")?;
            total += std::time::Duration::from_nanos(
                end.checked_sub(start)
                    .context("span ends before it starts")?,
            )
            .as_secs_f64()
                * 1_000.0;
        }
        Ok(total)
    };
    let sql = sum("wamn.postgres.statement", None)?;
    let root = sum("handle_http_request", None)?;
    Ok(json!({"passed":true,"phase":name,"http_total_ms":total_ms,
        "authentication_ms":sum("wamn.route.authenticate",None)?,"resolution_ms":sum("wamn.router.resolve",None)?,
        "artifact_pull_ms":sum("wamn.component.pull",None)?,"compile_ms":sum("wamn.component.compile",None)?,
        "linker_setup_ms":sum("wamn.component.linker_setup",None)?,"link_ms":sum("wamn.component.link",None)?,
        "executor_platform_acquire_ms":sum("wamn.postgres.acquire",Some("executor-platform"))?,
        "callable_http_acquire_ms":sum("wamn.postgres.acquire",Some("callable-http"))?,"guest_sql_acquire_ms":sum("wamn.postgres.acquire",Some("guest-sql"))?,
        "sql_ms":sql,"guest_db_call_ms":sum("wamn.postgres",None)?,"root_ms":root}))
}

async fn startup_time(
    cluster: &str,
    work: &Path,
    pod: &str,
    evidence: &Path,
    name: &str,
) -> anyhow::Result<u64> {
    let bytes = checked(kubectl(cluster, work).args([
        "-n",
        NAMESPACE,
        "logs",
        "--timestamps",
        pod,
        "-c",
        "host",
    ]))
    .await?;
    fs::write(evidence.join(name), &bytes)?;
    std::str::from_utf8(&bytes)?
        .lines()
        .filter(|line| line.contains("wamn-host runtime startup completed"))
        .flat_map(str::split_whitespace)
        .filter_map(|field| field.strip_prefix("elapsed_ms="))
        .next_back()
        .context("host did not report completed startup duration")?
        .parse()
        .context("host startup duration is a whole number of milliseconds")
}

async fn object(cluster: &str, work: &Path, kind: &str, name: &str) -> anyhow::Result<Value> {
    Ok(serde_json::from_slice(
        &checked(kubectl(cluster, work).args(["-n", NAMESPACE, "get", kind, name, "-o", "json"]))
            .await?,
    )?)
}

fn assert_native_probes(deployment: &Value) -> anyhow::Result<()> {
    ensure!(
        deployment["spec"]["replicas"] == 1 && deployment["status"]["availableReplicas"] == 1,
        "startup deployment requires one available replica"
    );
    let containers = deployment["spec"]["template"]["spec"]["containers"]
        .as_array()
        .context("host deployment has containers")?;
    ensure!(
        containers.len() == 1 && containers[0]["name"] == "host",
        "startup deployment requires its single native host container"
    );
    let container = &containers[0];
    ensure!(
        container["livenessProbe"]["httpGet"]
            == json!({"path":"/livez","port":"probes","scheme":"HTTP"})
            && container["readinessProbe"]["httpGet"]
                == json!({"path":"/readyz","port":"probes","scheme":"HTTP"})
            && container["startupProbe"]["httpGet"]
                == json!({"path":"/livez","port":"probes","scheme":"HTTP"}),
        "startup measurement must keep the native host probes"
    );
    ensure!(
        container["ports"].as_array().is_some_and(|ports| ports
            .iter()
            .any(|port| port["name"] == "probes" && port["containerPort"] == 8081)),
        "native probes must keep port 8081"
    );
    Ok(())
}
