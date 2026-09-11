//! Cold, restarted, and steady WMS requests retain their measured limits.

use std::fs;
use std::path::Path;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_gate_harness::journey::JourneyDocument;
use wamn_test_infrastructure::traces::{TraceDocument, request_trace_is_complete};

use super::deployment::{checked, kubectl};
use crate::wms_runtime_live::write_result;

#[derive(Debug)]
pub(super) struct ColdHost {
    pod: String,
    uid: String,
    startup_ms: u64,
}

pub(super) async fn cold_host(
    cluster: &str,
    work: &Path,
    evidence: &Path,
) -> anyhow::Result<ColdHost> {
    checked(kubectl(cluster, work).args([
        "-n",
        cluster,
        "scale",
        "deployment/hostgroup-default",
        "--replicas=1",
    ]))
    .await?;
    checked(kubectl(cluster, work).args([
        "-n",
        cluster,
        "wait",
        "--for=condition=Available",
        "deployment/hostgroup-default",
        "--timeout=120s",
    ]))
    .await?;
    let deployment = object(cluster, work, "deployment", "hostgroup-default").await?;
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
        &checked(
            kubectl(cluster, work)
                .args(["-n", cluster, "get", "pods", "-l", &selector, "-o", "json"]),
        )
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
        uid,
        startup_ms,
    })
}

pub(super) async fn requests(
    cluster: &str,
    work: &Path,
    image: &str,
    inputs: &JourneyDocument,
    cold: &ColdHost,
    evidence: &Path,
) -> anyhow::Result<()> {
    request(
        cluster,
        work,
        image,
        inputs,
        "cold",
        "11111111111111111111111111111111",
        "1111111111111111",
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
        cluster,
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
        cluster,
        "wait",
        "--for=jsonpath={.status.containerStatuses[0].restartCount}=1",
        &format!("pod/{}", cold.pod),
        "--timeout=120s",
    ]))
    .await?;
    checked(kubectl(cluster, work).args([
        "-n",
        cluster,
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
            cluster,
            "logs",
            "--previous",
            "--timestamps",
            &cold.pod,
            "-c",
            "host",
        ]))
        .await?,
    )?;
    route_after_restart(cluster, work, evidence).await?;
    let restart = request(
        cluster,
        work,
        image,
        inputs,
        "restart-first",
        "22222222222222222222222222222222",
        "2222222222222222",
        evidence,
    )
    .await?;
    let steady = request(
        cluster,
        work,
        image,
        inputs,
        "steady",
        "33333333333333333333333333333333",
        "3333333333333333",
        evidence,
    )
    .await?;
    trace(
        cluster,
        work,
        "restart-first",
        "22222222222222222222222222222222",
        restart,
        evidence,
    )
    .await?;
    let ratio = trace(
        cluster,
        work,
        "steady",
        "33333333333333333333333333333333",
        steady,
        evidence,
    )
    .await?
    .context("steady trace carries no statement or instantiate duration")?;
    ensure!(
        ratio <= 12.0,
        "steady request overhead ratio {ratio} exceeds 12"
    );
    write_result(
        evidence,
        "overhead-ratio-steady.json",
        &json!({"passed":true,"ratio":ratio,"ceiling":12}),
    )?;
    let warm_cache = cache_files(cluster, work, &cold.pod, evidence, "warm").await?;
    ensure!(
        cache == warm_cache,
        "the restarted host changed compiled cache bytes, inodes, times, sizes, or paths"
    );
    write_result(
        evidence,
        "runtime-startup.json",
        &json!({"passed":true,"pod_uid":cold.uid,"cold_startup_ms":cold.startup_ms,"restart_startup_ms":restarted_ms,"cache_entries":warm_cache.0.len()}),
    )
}

async fn request(
    cluster: &str,
    work: &Path,
    image: &str,
    inputs: &JourneyDocument,
    name: &str,
    trace_id: &str,
    parent_span: &str,
    evidence: &Path,
) -> anyhow::Result<f64> {
    let caller: Value = serde_json::from_slice(&fs::read(&inputs.route_caller_secret_output)?)?;
    let secret = caller["metadata"]["name"]
        .as_str()
        .context("route caller Secret has a name")?;
    let request_id = format!("startup-{name}");
    let body = serde_json::to_string(
        &json!([{"request_id":request_id,"id":super::application::PALLET_ID}]),
    )?;
    let job = format!("startup-request-{name}");
    let manifest = request_job(
        cluster,
        image,
        &inputs.route_host,
        secret,
        &job,
        trace_id,
        parent_span,
        &body,
    );
    let path = work.join(format!("{job}.json"));
    fs::write(&path, serde_json::to_vec_pretty(&manifest)?)?;
    checked(kubectl(cluster, work).args(["apply", "-f"]).arg(&path)).await?;
    let completed = checked(kubectl(cluster, work).args([
        "-n",
        cluster,
        "wait",
        "--for=condition=Complete",
        &format!("job/{job}"),
        "--timeout=240s",
    ]))
    .await;
    let pods: Value = serde_json::from_slice(
        &checked(kubectl(cluster, work).args([
            "-n",
            cluster,
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
        checked(kubectl(cluster, work).args(["-n", cluster, "logs", &format!("job/{job}")]))
            .await?,
    )?;
    completed?;
    ensure!(
        status["state"]["terminated"]["exitCode"] == 0,
        "request probe failed"
    );
    let total_ms = assert_response(&result, &request_id, name == "restart-first")?;
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

fn request_job(
    cluster: &str,
    image: &str,
    route_host: &str,
    secret: &str,
    job: &str,
    trace_id: &str,
    parent_span: &str,
    body: &str,
) -> Value {
    // The mounted PAT stays inside the owned request pod. Results contain no credentials.
    let script = r#"probe_start=$(date +%s)
attempt=0
while :; do
  attempt=$((attempt + 1))
  metrics=$(curl --silent --show-error --connect-timeout 5 --max-time 60 \
    --output /tmp/body --write-out '%{http_code} %{time_starttransfer} %{time_total}' \
    --header "Host: $ROUTE_HOST" --header 'Content-Type: application/json' \
    --header "Authorization: Bearer $ROUTE_CALLER_PAT" --header "traceparent: $TRACEPARENT" \
    --data "$REQUEST_BODY" "$ROUTE_URL") || metrics='000 0 0'
  set -- $metrics
  elapsed=$(( $(date +%s) - probe_start ))
  printf '{"attempt":%s,"status":"%s","recovery_seconds":%s,"total_seconds":"%s"}\n' "$attempt" "$1" "$elapsed" "$3"
  [ "$1" = 200 ] && break
  [ "$elapsed" -ge 150 ] && break
  sleep 1
done
body=$(od -An -v -tx1 /tmp/body | tr -d ' \n')
printf '{"status":"%s","first_seconds":"%s","total_seconds":"%s","recovery_seconds":%s,"attempts":%s,"body_hex":"%s"}\n' "$1" "$2" "$3" "$elapsed" "$attempt" "$body" >/dev/termination-log
test "$1" = 200
"#;
    json!({"apiVersion":"batch/v1","kind":"Job","metadata":{"name":job,"namespace":cluster},"spec":{"activeDeadlineSeconds":200,"backoffLimit":0,"template":{"spec":{"restartPolicy":"Never","containers":[{"name":"probe","image":image,"imagePullPolicy":"Never","terminationMessagePolicy":"File","command":["/bin/sh","-ec"],"args":[script],"env":[
        {"name":"ROUTE_CALLER_PAT","valueFrom":{"secretKeyRef":{"name":secret,"key":"token"}}},
        {"name":"ROUTE_HOST","value":route_host},{"name":"TRACEPARENT","value":format!("00-{trace_id}-{parent_span}-01")},
        {"name":"REQUEST_BODY","value":body},{"name":"ROUTE_URL","value":format!("http://flow-http.{cluster}.svc.cluster.local/pallet/get")}
    ]}]}}}})
}

fn assert_response(result: &Value, request_id: &str, restarted: bool) -> anyhow::Result<f64> {
    ensure!(
        result["status"] == "200",
        "startup request must return HTTP 200"
    );
    let seconds = |name: &str| -> anyhow::Result<f64> {
        let text = result[name].as_str().context("request timing is text")?;
        let (whole, fraction) = text
            .split_once('.')
            .context("request timing has a decimal point")?;
        ensure!(
            !whole.is_empty()
                && !fraction.is_empty()
                && whole
                    .bytes()
                    .chain(fraction.bytes())
                    .all(|byte| byte.is_ascii_digit()),
            "request timing is a nonnegative decimal"
        );
        let value: f64 = text.parse()?;
        ensure!(value.is_finite(), "request timing is finite");
        Ok(value)
    };
    seconds("first_seconds")?;
    let total = seconds("total_seconds")?;
    if restarted {
        ensure!(
            result["recovery_seconds"]
                .as_u64()
                .context("recovery is a whole number of seconds")?
                <= 120,
            "request recovery exceeded 120 seconds"
        );
    }
    let body: Value = serde_json::from_slice(&hex::decode(
        result["body_hex"]
            .as_str()
            .context("startup response has bytes")?,
    )?)?;
    let rows = body.as_array().context("startup response is an array")?;
    ensure!(
        rows.len() == 1
            && rows[0]["request_id"] == request_id
            && rows[0].get("error").is_none()
            && rows[0]["value"]["id"] == super::application::PALLET_ID,
        "startup response must return the requested WMS pallet without an error"
    );
    Ok(total * 1000.0)
}

async fn route_after_restart(cluster: &str, work: &Path, evidence: &Path) -> anyhow::Result<()> {
    let previous: Value =
        serde_json::from_slice(&fs::read(evidence.join("flow-http-replicaset.json"))?)?;
    let uid = previous["metadata"]["uid"]
        .as_str()
        .context("HTTP ReplicaSet has a UID")?;
    for _ in 0..120 {
        let workloads: Value = serde_json::from_slice(
            &checked(kubectl(cluster, work).args([
                "-n",
                cluster,
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
                cluster,
                "get",
                "endpointslices",
                "-l",
                "kubernetes.io/service-name=flow-http,wasmcloud.dev/route-manager=true",
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
            cluster,
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
            cluster,
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
) -> anyhow::Result<Option<f64>> {
    let bytes = checked(kubectl(cluster, work).args([
        "get",
        "--raw",
        &format!("/api/v1/namespaces/wamn-system/services/http:tempo:3200/proxy/api/traces/{id}"),
    ]))
    .await?;
    fs::write(evidence.join(format!("trace-{name}.json")), &bytes)?;
    let document: TraceDocument = serde_json::from_slice(&bytes)?;
    ensure!(
        request_trace_is_complete(&document, false, 1),
        "{name} trace must have one WMS statement and no executor acquisition or component loading"
    );
    let value: Value = serde_json::from_slice(&bytes)?;
    let breakdown = trace_breakdown(&value, name, total_ms)?;
    write_result(
        evidence,
        &format!("trace-breakdown-{name}.json"),
        &breakdown,
    )?;
    Ok(breakdown["overhead_ratio"].as_f64())
}

fn trace_breakdown(document: &Value, name: &str, total_ms: f64) -> anyhow::Result<Value> {
    let mut spans = Vec::new();
    for batch in document["batches"]
        .as_array()
        .context("trace has batches")?
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
            total += end
                .checked_sub(start)
                .context("span ends before it starts")? as f64
                / 1_000_000.0;
        }
        Ok(total)
    };
    let sql = sum("wamn.postgres.statement", None)?;
    let instantiate = sum("wamn.component.instantiate", None)?;
    let root = sum("handle_http_request", None)?;
    let work = sql + instantiate;
    Ok(json!({"passed":true,"phase":name,"http_total_ms":total_ms,
        "authentication_ms":sum("wamn.route.authenticate",None)?,"resolution_ms":sum("wamn.router.resolve",None)?,
        "artifact_pull_ms":sum("wamn.component.pull",None)?,"compile_ms":sum("wamn.component.compile",None)?,
        "linker_setup_ms":sum("wamn.component.linker_setup",None)?,"link_ms":sum("wamn.component.link",None)?,
        "instantiate_ms":instantiate,"executor_platform_acquire_ms":sum("wamn.postgres.acquire",Some("executor-platform"))?,
        "callable_http_acquire_ms":sum("wamn.postgres.acquire",Some("callable-http"))?,"guest_sql_acquire_ms":sum("wamn.postgres.acquire",Some("guest-sql"))?,
        "sql_ms":sql,"guest_db_call_ms":sum("wamn.postgres",None)?,"root_ms":root,"real_work_ms":work,
        "overhead_ratio":if work > 0.0 {Some(root/work)} else {None}}))
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
        cluster,
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
        .last()
        .context("host did not report completed startup duration")?
        .parse()
        .context("host startup duration is a whole number of milliseconds")
}

async fn object(cluster: &str, work: &Path, kind: &str, name: &str) -> anyhow::Result<Value> {
    Ok(serde_json::from_slice(
        &checked(kubectl(cluster, work).args(["-n", cluster, "get", kind, name, "-o", "json"]))
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

fn assert_host_restart(pod: &Value, uid: Option<&str>, restarts: u64) -> anyhow::Result<()> {
    ensure!(
        uid.is_none_or(|uid| pod["metadata"]["uid"] == uid),
        "host restart replaced the measured pod"
    );
    ensure!(
        pod["status"]["containerStatuses"]
            .as_array()
            .is_some_and(
                |statuses| statuses.iter().any(|status| status["name"] == "host"
                    && status["ready"] == true
                    && status["restartCount"].as_u64() == Some(restarts))
            ),
        "measured host must be ready with the exact restart count"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_job_reports_immediate_delayed_and_failed_responses() {
        use std::os::unix::fs::PermissionsExt as _;
        let body = r#"[{"request_id":"startup-cold","id":"selected-pallet","note":"it's quoted"}]"#;
        let job = request_job(
            "selected-environment",
            "selected-host-image",
            "selected.example",
            "selected-pat",
            "selected-job",
            "11111111111111111111111111111111",
            "1111111111111111",
            body,
        );
        assert_eq!(
            job["metadata"],
            json!({"name":"selected-job","namespace":"selected-environment"})
        );
        assert_eq!(job["spec"]["activeDeadlineSeconds"], 200);
        assert_eq!(job["spec"]["backoffLimit"], 0);
        assert_eq!(job["spec"]["template"]["spec"]["restartPolicy"], "Never");
        let containers = job["spec"]["template"]["spec"]["containers"]
            .as_array()
            .unwrap();
        assert_eq!(containers.len(), 1);
        let container = &containers[0];
        assert_eq!(container["image"], "selected-host-image");
        assert_eq!(container["imagePullPolicy"], "Never");
        assert_eq!(container["command"], json!(["/bin/sh", "-ec"]));
        let env = container["env"].as_array().unwrap();
        assert_eq!(
            env[0],
            json!({"name":"ROUTE_CALLER_PAT","valueFrom":{"secretKeyRef":{"name":"selected-pat","key":"token"}}})
        );
        assert_eq!(
            env[1],
            json!({"name":"ROUTE_HOST","value":"selected.example"})
        );
        assert_eq!(
            env[2],
            json!({"name":"TRACEPARENT","value":"00-11111111111111111111111111111111-1111111111111111-01"})
        );
        assert_eq!(env[3], json!({"name":"REQUEST_BODY","value":body}));
        assert_eq!(
            env[4],
            json!({"name":"ROUTE_URL","value":"http://flow-http.selected-environment.svc.cluster.local/pallet/get"})
        );
        assert_eq!(env.len(), 5);
        let script = container["args"][0].as_str().unwrap();
        assert!(script.contains("--connect-timeout 5 --max-time 60"));
        assert_eq!(script.matches("-ge 150").count(), 1);

        for (name, codes, succeeds, expected_attempts) in [
            ("immediate", "200", true, 1),
            ("delayed", "404 404 200", true, 3),
            ("failed", "404", false, 0),
        ] {
            let directory = wamn_test_infrastructure::scratch::ScratchRoot(
                std::env::temp_dir().join(format!("wms-startup-test-{}", uuid::Uuid::new_v4())),
            );
            fs::create_dir(directory.path()).unwrap();
            let program = directory.path().join("curl");
            fs::write(
                &program,
                r#"#!/bin/sh
set -eu
count=0
if [ -f "$TEST_DIRECTORY/count" ]; then count=$(cat "$TEST_DIRECTORY/count"); fi
count=$((count + 1))
printf '%s\n' "$count" >"$TEST_DIRECTORY/count"
output=''
previous=''
for argument in "$@"; do
  if [ "$previous" = --output ]; then output=$argument; fi
  previous=$argument
done
test -n "$output"
index=0
status=''
for value in $TEST_CODES; do
  index=$((index + 1))
  status=$value
  if [ "$index" -eq "$count" ]; then break; fi
done
printf 'test-body-%s' "$status" >"$output"
printf '%s 0.001 0.002' "$status"
"#,
            )
            .unwrap();
            fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
            let script = script
                .replace(
                    "/tmp/body",
                    &directory.path().join("body").display().to_string(),
                )
                .replace(
                    "/dev/termination-log",
                    &directory.path().join("result.json").display().to_string(),
                );
            // The retained failed-response test shortens only the retry window.
            let script = if succeeds {
                script
            } else {
                script.replace("-ge 150", "-ge 2")
            };
            let mut command = std::process::Command::new("sh");
            command
                .args(["-ec", &script])
                .env(
                    "PATH",
                    format!(
                        "{}:{}",
                        directory.path().display(),
                        std::env::var("PATH").unwrap()
                    ),
                )
                .env("TEST_DIRECTORY", directory.path())
                .env("TEST_CODES", codes)
                .env("ROUTE_CALLER_PAT", "local-test-token");
            for entry in &env[1..] {
                command.env(
                    entry["name"].as_str().unwrap(),
                    entry["value"].as_str().unwrap(),
                );
            }
            let output = command.output().unwrap();
            assert_eq!(
                output.status.success(),
                succeeds,
                "{name}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let lines = String::from_utf8(output.stdout)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .collect::<Vec<_>>();
            let result: Value =
                serde_json::from_slice(&fs::read(directory.path().join("result.json")).unwrap())
                    .unwrap();
            assert_eq!(result["attempts"].as_u64().unwrap(), lines.len() as u64);
            assert_eq!(result["status"], if succeeds { "200" } else { "404" });
            assert_eq!(result["first_seconds"], "0.001");
            assert_eq!(result["total_seconds"], "0.002");
            assert_eq!(
                hex::decode(result["body_hex"].as_str().unwrap()).unwrap(),
                if succeeds {
                    b"test-body-200"
                } else {
                    b"test-body-404"
                }
            );
            if succeeds {
                assert_eq!(lines.len(), expected_attempts);
            } else {
                assert!(lines.len() >= 2);
                assert!(result["recovery_seconds"].as_u64().unwrap() >= 2);
            }
            for (index, attempt) in lines.iter().enumerate() {
                assert_eq!(attempt["attempt"], index + 1);
                assert!(attempt["recovery_seconds"].is_u64());
                assert_eq!(
                    attempt["status"],
                    if succeeds && index == lines.len() - 1 {
                        "200"
                    } else {
                        "404"
                    }
                );
            }
        }
    }

    #[test]
    fn startup_request_keeps_the_recovery_limit_and_pallet_identity() {
        let body = json!([{"request_id":"startup-restart-first","value":{"id":super::super::application::PALLET_ID}}]);
        let mut result = json!({"status":"200","first_seconds":"0.010","total_seconds":"0.020","recovery_seconds":120,"body_hex":hex::encode(serde_json::to_vec(&body).unwrap())});
        assert_eq!(
            assert_response(&result, "startup-restart-first", true).unwrap(),
            20.0
        );
        result["recovery_seconds"] = json!(121);
        assert!(assert_response(&result, "startup-restart-first", true).is_err());
        result["recovery_seconds"] = json!(120);
        assert!(assert_response(&result, "another-request", true).is_err());
        result["status"] = json!("503");
        assert!(assert_response(&result, "startup-restart-first", true).is_err());
    }

    #[test]
    fn restarting_a_replaced_or_unready_pod_does_not_pass() {
        let mut pod = json!({"metadata":{"uid":"same"},"status":{"containerStatuses":[{"name":"host","ready":true,"restartCount":1}]}});
        assert!(assert_host_restart(&pod, Some("same"), 1).is_ok());
        assert!(assert_host_restart(&pod, Some("different"), 1).is_err());
        pod["status"]["containerStatuses"][0]["restartCount"] = json!(2);
        assert!(assert_host_restart(&pod, Some("same"), 1).is_err());
        pod["status"]["containerStatuses"][0]["restartCount"] = json!(1);
        pod["status"]["containerStatuses"][0]["ready"] = json!(false);
        assert!(assert_host_restart(&pod, Some("same"), 1).is_err());
    }
}
