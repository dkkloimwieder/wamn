//! Receiving startup, throughput, and fresh-authority measurements.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt as _;
use tokio::process::Command;
use wamn_proof_integration::throughput_bench::{self, LayerSpec, StepSpec, ThroughputIndex};
use wamn_test_infrastructure::traces::{TraceDocument, request_trace_is_complete};

use super::resources::checked;
use super::{ReceivingCluster, resources};

const PURCHASE_ORDER_ID: &str = "00000000-0000-0000-0000-000000000301";

fn kubectl(cluster: &str, work: &Path) -> Command {
    let mut command = Command::new("kubectl");
    command
        .arg("--kubeconfig")
        .arg(work.join("kubeconfig"))
        .arg("--context")
        .arg(format!("kind-{cluster}"));
    command
}

fn write_result(directory: &Path, name: &str, result: &Value) -> anyhow::Result<()> {
    fs::write(directory.join(name), serde_json::to_vec_pretty(result)?)?;
    Ok(())
}

#[derive(Debug)]
pub(super) struct ColdHost {
    pod: String,
    uid: String,
    startup_ms: u64,
}

pub(super) async fn cold_host(state: &ReceivingCluster) -> anyhow::Result<ColdHost> {
    let cluster = state.resources.name.as_str();
    let work = state.resources.work.as_path();
    let evidence = state.resources.evidence.as_path();
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

pub(super) async fn measure_startup(
    state: &ReceivingCluster,
    cold: &ColdHost,
) -> anyhow::Result<()> {
    requests(state, cold, None, false).await
}

pub(super) async fn throughput(
    state: &ReceivingCluster,
    cold: &ColdHost,
    project_database_url: &str,
) -> anyhow::Result<()> {
    requests(state, cold, Some(project_database_url), false).await
}

pub(super) async fn fresh_auth(
    state: &ReceivingCluster,
    cold: &ColdHost,
    project_database_url: &str,
) -> anyhow::Result<()> {
    requests(state, cold, Some(project_database_url), true).await
}

async fn requests(
    state: &ReceivingCluster,
    cold: &ColdHost,
    project_database_url: Option<&str>,
    fresh_auth: bool,
) -> anyhow::Result<()> {
    let cluster = state.resources.name.as_str();
    let work = state.resources.work.as_path();
    let evidence = state.resources.evidence.as_path();
    let inputs = &state.inputs;
    let caller: Value = serde_json::from_slice(&fs::read(&inputs.route_caller_secret_output)?)?;
    let endpoint =
        super::materializer_case::endpoint(state, "receiving-measurement-nodeport").await?;
    let service_token = caller["stringData"]["token"]
        .as_str()
        .context("route caller Secret has a token")?;
    let service_secret = caller["metadata"]["name"]
        .as_str()
        .context("route caller Secret has a name")?;
    request(
        state,
        &endpoint,
        "cold",
        "startup-cold",
        service_token,
        "11111111111111111111111111111111",
        "1111111111111111",
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
    let endpoint =
        super::materializer_case::endpoint(state, "receiving-measurement-nodeport").await?;
    let restart = request(
        state,
        &endpoint,
        "restart-first",
        "startup-restart-first",
        service_token,
        "22222222222222222222222222222222",
        "2222222222222222",
    )
    .await?;
    let steady = request(
        state,
        &endpoint,
        "steady",
        "startup-steady",
        service_token,
        "33333333333333333333333333333333",
        "3333333333333333",
    )
    .await?;
    let mut additional = Vec::new();
    for sample in 2..=5 {
        let name = format!("steady-{sample}");
        let trace_id = format!("3333333333333333333333333333000{sample}");
        let duration = request(
            state,
            &endpoint,
            &name,
            &format!("startup-{name}"),
            service_token,
            &trace_id,
            &format!("333333333333000{sample}"),
        )
        .await?;
        additional.push((name, trace_id, duration));
    }
    trace(
        cluster,
        work,
        "restart-first",
        "22222222222222222222222222222222",
        restart,
        evidence,
    )
    .await?;
    let mut ratios = vec![
        trace(
            cluster,
            work,
            "steady",
            "33333333333333333333333333333333",
            steady,
            evidence,
        )
        .await?
        .context("steady trace has no measured statement or instantiate duration")?,
    ];
    for (name, id, duration) in additional {
        ratios.push(
            trace(cluster, work, &name, &id, duration, evidence)
                .await?
                .context("steady trace has no measured statement or instantiate duration")?,
        );
    }
    if let Some(project_database_url) = project_database_url {
        if fresh_auth {
            human_runs(state, cold, project_database_url, service_secret, &endpoint).await?;
        } else {
            throughput_sweep(
                state,
                cold,
                project_database_url,
                &evidence.join("throughput"),
                "service",
                1,
                service_secret,
            )
            .await?;
        }
    }
    let median = overhead_median(&ratios)?;
    write_result(
        evidence,
        "overhead-ratio-steady.json",
        &json!({"passed":true,"phase":"steady","overhead_ratio":median,"ceiling":12,"samples":ratios.len(),"ratios":ratios}),
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
    state: &ReceivingCluster,
    endpoint: &str,
    name: &str,
    request_id: &str,
    token: &str,
    trace_id: &str,
    parent_span: &str,
) -> anyhow::Result<f64> {
    let resources = &state.resources;
    let evidence = &resources.evidence;
    let body = serde_json::to_string(&json!([{"request_id":request_id,"id":PURCHASE_ORDER_ID}]))?;
    ensure!(
        !token.contains(['\r', '\n']) && !state.inputs.route_host.contains(['\r', '\n']),
        "request headers must not contain a line break"
    );
    let quote = |value: &str| value.replace('\\', "\\\\").replace('"', "\\\"");
    // The credential crosses only curl's private stdin. Results never include its configuration.
    let configuration = format!(
        "url = \"{}/purchase_order/get\"\nheader = \"Host: {}\"\nheader = \"Content-Type: application/json\"\nheader = \"Authorization: Bearer {}\"\nheader = \"traceparent: 00-{trace_id}-{parent_span}-01\"\ndata = \"{}\"\n",
        quote(endpoint),
        quote(&state.inputs.route_host),
        quote(token),
        quote(&body),
    );
    let response_path = resources
        .work
        .join(format!("first-request-{name}-body.json"));
    let attempts_path = evidence.join(format!("first-request-{name}-attempts.jsonl"));
    let mut attempts = String::new();
    let mut diagnostics = Vec::new();
    let mut attempt = 0;
    let started = Instant::now();
    let measured = tokio::time::timeout(Duration::from_secs(200), async {
        loop {
            attempt += 1;
            let mut child = Command::new("curl")
                .args(["--disable", "--config", "-", "--silent", "--show-error", "--noproxy", "*",
                    "--connect-timeout", "5", "--max-time", "60", "--output"])
                .arg(&response_path)
                .args(["--write-out", "%{http_code} %{time_starttransfer} %{time_total}"])
                .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
                .kill_on_drop(true).spawn().context("start the request measurement")?;
            let mut input = child.stdin.take().context("curl has private input")?;
            input.write_all(configuration.as_bytes()).await?;
            drop(input);
            let output = child.wait_with_output().await?;
            diagnostics.extend_from_slice(&output.stderr);
            let metrics = if output.status.success() {
                String::from_utf8(output.stdout).context("curl timings are text")?
            } else {
                "000 0 0".to_owned()
            };
            let fields = metrics.split_whitespace().collect::<Vec<_>>();
            ensure!(fields.len() == 3, "curl must return status, first-byte time and total time");
            let elapsed = started.elapsed().as_secs();
            attempts.push_str(&serde_json::to_string(&json!({"attempt":attempt,"status":fields[0],
                "recovery_seconds":elapsed,"total_seconds":fields[2],"origin":"host-nodeport","endpoint":endpoint}))?);
            attempts.push('\n');
            fs::write(&attempts_path, &attempts)?;
            fs::write(evidence.join(format!("first-request-{name}.stderr")), &diagnostics)?;
            if fields[0] == "200" || elapsed >= 150 {
                let response = fs::read(&response_path).unwrap_or_default();
                return Ok::<_,anyhow::Error>(json!({"status":fields[0],"first_seconds":fields[1],
                    "total_seconds":fields[2],"recovery_seconds":elapsed,"attempts":attempt,
                    "body_hex":hex::encode(response),"origin":"host-nodeport","endpoint":endpoint}));
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }).await.context("request measurement exceeded its retained 200-second deadline")?;
    let result = measured?;
    write_result(evidence, &format!("first-request-{name}.json"), &result)?;
    let total_ms = assert_response(&result, request_id, name == "restart-first")?;
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
            && rows[0]["value"]["id"] == PURCHASE_ORDER_ID,
        "startup response must return the requested Receiving purchase order without an error"
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
        "{name} trace must have one Receiving statement and no executor acquisition or component loading"
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
    let identity_reads = spans
        .iter()
        .filter(|span| {
            matches!(
                span["name"].as_str(),
                Some(
                    "wamn.auth.pat"
                        | "wamn.auth.roles"
                        | "wamn.auth.membership"
                        | "wamn.auth.identity"
                )
            )
        })
        .count();
    let permission_reads = spans
        .iter()
        .filter(|span| span["name"] == "wamn.auth.permissions")
        .count();
    let sql = sum("wamn.postgres.statement", None)?;
    let instantiate = sum("wamn.component.instantiate", None)?;
    let root = sum("handle_http_request", None)?;
    let work = sql + instantiate;
    Ok(
        json!({"passed":true,"phase":name,"http_total_ms":total_ms,"http_origin":"host-nodeport",
        "authentication_ms":sum("wamn.route.authenticate",None)?,
        "identity_read_spans":identity_reads,"permission_read_spans":permission_reads,
        "resolution_ms":sum("wamn.router.resolve",None)?,
        "artifact_pull_ms":sum("wamn.component.pull",None)?,"compile_ms":sum("wamn.component.compile",None)?,
        "linker_setup_ms":sum("wamn.component.linker_setup",None)?,"link_ms":sum("wamn.component.link",None)?,
        "instantiate_ms":instantiate,"executor_platform_acquire_ms":sum("wamn.postgres.acquire",Some("executor-platform"))?,
        "callable_http_acquire_ms":sum("wamn.postgres.acquire",Some("callable-http"))?,"guest_sql_acquire_ms":sum("wamn.postgres.acquire",Some("guest-sql"))?,
        "sql_ms":sql,"guest_db_call_ms":sum("wamn.postgres",None)?,"root_ms":root,"real_work_ms":work,
        "overhead_ratio":if work > 0.0 {Some(root/work)} else {None}}),
    )
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

fn overhead_median(ratios: &[f64]) -> anyhow::Result<f64> {
    ensure!(
        ratios.len() == 5
            && ratios
                .iter()
                .all(|value| value.is_finite() && *value >= 0.0),
        "the Receiving overhead limit requires five finite steady ratios"
    );
    let mut sorted = ratios.to_vec();
    sorted.sort_by(f64::total_cmp);
    let median = sorted[2];
    ensure!(
        median <= 12.0,
        "steady request overhead median {median} exceeds 12"
    );
    Ok(median)
}

async fn human_runs(
    state: &ReceivingCluster,
    cold: &ColdHost,
    project_database_url: &str,
    service_secret: &str,
    endpoint: &str,
) -> anyhow::Result<()> {
    let resources = &state.resources;
    let human_pat = resources.work.join("throughput-human-pat");
    let human_secret = "wamn-throughput-human";
    wamn_proof_integration::membershipproof::run(
        wamn_proof_integration::membershipproof::MembershipProofArgs {
            system_database_url: state.inputs.system_pg_url.clone(),
            project_database_url: project_database_url.to_owned(),
            endpoint_url: format!("http://flow-http.{}.svc.cluster.local", resources.name),
            host: state.inputs.route_host.clone(),
            org: super::super::ORG.to_owned(),
            project: super::super::PROJECT.to_owned(),
            env: super::super::ENVIRONMENT.to_owned(),
            tenant: super::super::TENANT.to_owned(),
            throughput_pat_file: Some(human_pat.clone()),
        },
    )
    .await?;
    ensure!(
        fs::metadata(&human_pat)?.permissions().mode() & 0o777 == 0o600,
        "the human PAT file must be private"
    );
    write_result(
        &resources.evidence,
        "human-benchmark-fixture.json",
        &json!({"ready":true,"credential":"human-pat","cleanup":"owned-cluster"}),
    )?;
    checked(
        super::kubectl(resources)
            .args([
                "-n",
                &resources.name,
                "create",
                "secret",
                "generic",
                human_secret,
            ])
            .arg(format!("--from-file=token={}", human_pat.display())),
    )
    .await?;
    let human_token = fs::read_to_string(&human_pat)?;
    let mut observations = Vec::new();
    for sample in 1..=5 {
        let name = format!("human-{sample}");
        let id = format!("4444444444444444444444444444000{sample}");
        let elapsed = request(
            state,
            endpoint,
            &name,
            &name,
            &human_token,
            &id,
            &format!("444444444444000{sample}"),
        )
        .await?;
        observations.push((name, id, elapsed));
    }
    for (name, id, elapsed) in observations {
        trace(
            &resources.name,
            &resources.work,
            &name,
            &id,
            elapsed,
            &resources.evidence,
        )
        .await?;
    }
    let mut order = Vec::new();
    for (credential, repetition, secret) in fresh_runs(service_secret, human_secret) {
        let output = resources
            .evidence
            .join("throughput")
            .join(format!("{credential}-{repetition}"));
        throughput_sweep(
            state,
            cold,
            project_database_url,
            &output,
            credential,
            repetition,
            secret,
        )
        .await?;
        order.push(json!({"credential":credential,"repetition":repetition}));
    }
    write_result(
        &resources.evidence,
        "fresh-auth-runs.json",
        &json!({"completed":order}),
    )
}

fn fresh_runs<'a>(
    service_secret: &'a str,
    human_secret: &'a str,
) -> Vec<(&'static str, u32, &'a str)> {
    let mut runs = Vec::new();
    for repetition in 1..=3 {
        let credentials = if repetition == 2 {
            ["human", "service"]
        } else {
            ["service", "human"]
        };
        for credential in credentials {
            let secret = if credential == "human" {
                human_secret
            } else {
                service_secret
            };
            runs.push((credential, repetition, secret));
        }
    }
    runs
}

fn benchmark_sql(generated: &str) -> anyhow::Result<String> {
    let rewritten = generated
        .lines()
        .map(|line| {
            if line == "FROM purchase_order AS model" {
                "FROM receiving.purchase_order AS model".to_owned()
            } else if let Some(prefix) = line.strip_suffix("= $1::uuid;") {
                format!("{prefix}= '{PURCHASE_ORDER_ID}'::uuid;")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    ensure!(
        rewritten.contains("FROM receiving.purchase_order AS model")
            && rewritten.contains(&format!("'{PURCHASE_ORDER_ID}'::uuid")),
        "the throughput statement did not derive from the generated Receiving read"
    );
    Ok(rewritten)
}

async fn throughput_sweep(
    state: &ReceivingCluster,
    cold: &ColdHost,
    project_database_url: &str,
    evidence: &Path,
    credential: &str,
    repetition: u32,
    secret: &str,
) -> anyhow::Result<()> {
    fs::create_dir_all(evidence)?;
    let resources = &state.resources;
    let postgres = resources::inspect(resources, "postgres").await?;
    let postgres_host = resources::kind_address(&postgres)?.to_string();
    let postgres_port = resources::postgres_host_port(&postgres)?;
    let sample_url = format!("postgresql://postgres:probe@127.0.0.1:{postgres_port}/postgres");
    let database: tokio_postgres::Config = project_database_url.parse()?;
    let database = database
        .get_dbname()
        .context("the throughput project URL names its database")?;
    let statement = benchmark_sql(&fs::read_to_string(
        resources
            .repository
            .join("apps/wamn_receiving/generated/sql/purchase_order/get.sql"),
    )?)?;
    let mut index = ThroughputIndex {
        schema:throughput_bench::INDEX_SCHEMA.to_owned(),source:resources.source.clone(),duration_seconds:10,
        concurrency:vec![1,4,8,16,32,64],
        layers:vec![
            LayerSpec {layer:"route".to_owned(),driver:"oha".to_owned(),target:format!("POST /purchase_order/get through flow-http, {credential} PAT, repetition {repetition}"),expected_status:Some(200)},
            LayerSpec {layer:"nodb".to_owned(),driver:"oha".to_owned(),target:"GET /no-such-route through flow-http: routed and answered 404 by the guest, no auth, no database".to_owned(),expected_status:Some(404)},
            LayerSpec {layer:"pg".to_owned(),driver:"pgbench".to_owned(),target:format!("pgbench -M prepared, the generated purchase_order/get read against {database} as postgres"),expected_status:None},
        ],steps:Vec::new(),
    };
    let mut lines = String::new();
    for layer in ["route", "nodb", "pg"] {
        for concurrency in &index.concurrency {
            let job = format!("bench-{credential}-r{repetition}-{layer}-c{concurrency}");
            let manifest = throughput_job(
                &state.resources.name,
                &state.inputs.route_host,
                &job,
                layer,
                *concurrency,
                secret,
                &postgres_host,
                database,
                &statement,
            )?;
            let path = resources.work.join(format!("{job}.json"));
            fs::write(&path, serde_json::to_vec_pretty(&manifest)?)?;
            take_sample(
                state,
                cold,
                &sample_url,
                database,
                evidence,
                layer,
                *concurrency,
                "before",
            )
            .await?;
            checked(super::kubectl(resources).args(["apply", "-f"]).arg(&path)).await?;
            let completed = checked(super::kubectl(resources).args([
                "-n",
                &resources.name,
                "wait",
                "--for=condition=Complete",
                &format!("job/{job}"),
                "--timeout=150s",
            ]))
            .await;
            let log = super::kubectl(resources)
                .args(["-n", &resources.name, "logs", &format!("job/{job}")])
                .kill_on_drop(true)
                .output()
                .await?;
            fs::write(
                evidence.join(format!("{layer}-c{concurrency}.out")),
                &log.stdout,
            )?;
            fs::write(
                evidence.join(format!("{layer}-c{concurrency}.stderr")),
                &log.stderr,
            )?;
            if let Err(error) = completed {
                let pods = super::kubectl(resources)
                    .args([
                        "-n",
                        &resources.name,
                        "get",
                        "pods",
                        "-l",
                        &format!("job-name={job}"),
                        "-o",
                        "json",
                    ])
                    .kill_on_drop(true)
                    .output()
                    .await?;
                fs::write(
                    evidence.join(format!("{layer}-c{concurrency}-pods.json")),
                    pods.stdout,
                )?;
                return Err(error.context(format!("throughput step {job} did not complete")));
            }
            ensure!(
                log.status.success(),
                "the completed throughput Job did not return its generator output"
            );
            take_sample(
                state,
                cold,
                &sample_url,
                database,
                evidence,
                layer,
                *concurrency,
                "after",
            )
            .await?;
            let step = StepSpec {
                layer: layer.to_owned(),
                concurrency: *concurrency,
                result: format!("{layer}-c{concurrency}.out"),
                before: format!("sample-{layer}-c{concurrency}-before.json"),
                after: format!("sample-{layer}-c{concurrency}-after.json"),
                host_cpu_before: format!("cpu-host-{layer}-c{concurrency}-before.txt"),
                host_cpu_after: format!("cpu-host-{layer}-c{concurrency}-after.txt"),
                pg_cpu_before: format!("cpu-pg-{layer}-c{concurrency}-before.txt"),
                pg_cpu_after: format!("cpu-pg-{layer}-c{concurrency}-after.txt"),
            };
            lines.push_str(&serde_json::to_string(&step)?);
            lines.push('\n');
            fs::write(evidence.join("steps.jsonl"), &lines)?;
            index.steps.push(step);
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
    fs::write(
        evidence.join(throughput_bench::INDEX_FILE),
        serde_json::to_vec_pretty(&index)?,
    )?;
    let report = throughput_bench::build_report(evidence)?;
    fs::write(evidence.join("report.md"), report.render_markdown())?;
    fs::write(
        evidence.join("summary.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    fs::write(
        evidence.join("verdict.json"),
        serde_json::to_vec_pretty(&report.verdicts)?,
    )?;
    Ok(())
}

fn throughput_job(
    namespace: &str,
    host: &str,
    job: &str,
    layer: &str,
    concurrency: u32,
    secret: &str,
    postgres: &str,
    database: &str,
    statement: &str,
) -> anyhow::Result<Value> {
    ensure!(concurrency > 0, "throughput concurrency must be positive");
    let oha =
        "ghcr.io/hatoo/oha@sha256:3ec3dbf549ea197793482d47a6324797411406bbf438c2fe8b91f244ec641a2f";
    let postgres_image =
        "postgres@sha256:7157393f508fd8eb46119937fab39813783fe3e7d4c6316c45c12ce2ea25e61d";
    let container = match layer {
        "route" => json!({"name":"oha","image":oha,"imagePullPolicy":"IfNotPresent",
            "env":[{"name":"ROUTE_CALLER_PAT","valueFrom":{"secretKeyRef":{"name":secret,"key":"token"}}}],"command":["/bin/oha"],
            "args":["--no-tui","-z","10s","-c",concurrency.to_string(),"--output-format","json","-m","POST","-H",format!("Host: {host}"),"-H","Content-Type: application/json","-H","Authorization: Bearer $(ROUTE_CALLER_PAT)","-d",serde_json::to_string(&json!([{"request_id":"bench","id":PURCHASE_ORDER_ID}]))?,format!("http://flow-http.{namespace}.svc.cluster.local/purchase_order/get")]}),
        "nodb" => {
            json!({"name":"oha","image":oha,"imagePullPolicy":"IfNotPresent","command":["/bin/oha"],
            "args":["--no-tui","-z","10s","-c",concurrency.to_string(),"--output-format","json","-m","GET","-H",format!("Host: {host}"),format!("http://flow-http.{namespace}.svc.cluster.local/no-such-route")]})
        }
        "pg" => {
            let script = format!(
                "cat >/tmp/bench.sql <<'SQL'\n{statement}\nSQL\npgbench -h {postgres} -p 5432 -U postgres -d {database} \\\n  -n -M prepared -c {concurrency} -j {} -T 10 \\\n  -f /tmp/bench.sql -l --log-prefix /tmp/pgb --sampling-rate 0.05\necho {}\ncat /tmp/pgb*\n",
                concurrency.min(8),
                throughput_bench::PGBENCH_LOG_MARKER
            );
            json!({"name":"pgbench","image":postgres_image,"imagePullPolicy":"IfNotPresent","env":[{"name":"PGPASSWORD","value":"probe"}],"command":["/bin/sh","-ec"],"args":[script]})
        }
        _ => anyhow::bail!("unknown Receiving throughput layer {layer}"),
    };
    Ok(
        json!({"apiVersion":"batch/v1","kind":"Job","metadata":{"name":job,"namespace":namespace,"labels":{"wamn.bench/layer":layer,"wamn.bench/concurrency":concurrency.to_string()}},"spec":{"activeDeadlineSeconds":120,"backoffLimit":0,"template":{"spec":{"restartPolicy":"Never","containers":[container]}}}}),
    )
}

async fn take_sample(
    state: &ReceivingCluster,
    cold: &ColdHost,
    postgres_url: &str,
    database: &str,
    evidence: &Path,
    layer: &str,
    concurrency: u32,
    position: &str,
) -> anyhow::Result<()> {
    let resources = &state.resources;
    let mut sample = throughput_bench::sample(
        postgres_url,
        &["postgres".to_owned(), database.to_owned()],
        None,
    )
    .await?;
    let monitor = Command::new(&resources.lifecycle)
        .args(["broker-varz", &format!("{}-nats", resources.name)])
        .kill_on_drop(true)
        .output()
        .await?;
    if monitor.status.success() {
        sample.nats = Some(serde_json::from_slice(&monitor.stdout)?);
        sample.nats_error = None;
    } else {
        sample.nats = None;
        sample.nats_error = Some("broker loopback monitoring request failed".to_owned());
    }
    fs::write(
        evidence.join(format!("sample-{layer}-c{concurrency}-{position}.json")),
        serde_json::to_vec_pretty(&sample)?,
    )?;
    fs::write(
        evidence.join(format!("cpu-host-{layer}-c{concurrency}-{position}.txt")),
        checked(super::kubectl(resources).args([
            "-n",
            &resources.name,
            "exec",
            &cold.pod,
            "-c",
            "host",
            "--",
            "cat",
            "/sys/fs/cgroup/cpu.stat",
        ]))
        .await?,
    )?;
    fs::write(
        evidence.join(format!("cpu-pg-{layer}-c{concurrency}-{position}.txt")),
        checked(
            Command::new(&resources.lifecycle)
                .args(["container-cpu", &format!("{}-postgres", resources.name)]),
        )
        .await?,
    )?;
    fs::write(
        evidence.join(format!("load-{layer}-c{concurrency}-{position}.txt")),
        fs::read("/proc/loadavg")?,
    )?;
    fs::write(
        evidence.join(format!(
            "memory-machine-{layer}-c{concurrency}-{position}.txt"
        )),
        fs::read("/proc/meminfo")?,
    )?;
    fs::write(
        evidence.join(format!("memory-host-{layer}-c{concurrency}-{position}.txt")),
        checked(super::kubectl(resources).args([
            "-n",
            &resources.name,
            "exec",
            &cold.pod,
            "-c",
            "host",
            "--",
            "cat",
            "/sys/fs/cgroup/memory.current",
        ]))
        .await?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered_job(layer: &str, concurrency: u32) -> anyhow::Result<Value> {
        throughput_job(
            "selected-environment",
            "selected.example",
            "selected-job",
            layer,
            concurrency,
            "selected-pat",
            "10.89.0.7",
            "selected_database",
            "SELECT model.id FROM receiving.purchase_order AS model WHERE model.id = '00000000-0000-0000-0000-000000000301'::uuid;",
        )
    }

    #[test]
    fn throughput_jobs_keep_the_selected_http_fields_and_private_pat_reference() {
        let route = rendered_job("route", 4).unwrap();
        assert_eq!(
            route["metadata"],
            json!({"name":"selected-job","namespace":"selected-environment","labels":{"wamn.bench/layer":"route","wamn.bench/concurrency":"4"}})
        );
        assert_eq!(route["kind"], "Job");
        assert_eq!(route["spec"]["activeDeadlineSeconds"], 120);
        assert_eq!(route["spec"]["backoffLimit"], 0);
        assert_eq!(route["spec"]["template"]["spec"]["restartPolicy"], "Never");
        let containers = route["spec"]["template"]["spec"]["containers"]
            .as_array()
            .unwrap();
        assert_eq!(containers.len(), 1);
        let container = &containers[0];
        assert_eq!(
            container["image"],
            "ghcr.io/hatoo/oha@sha256:3ec3dbf549ea197793482d47a6324797411406bbf438c2fe8b91f244ec641a2f"
        );
        assert_eq!(container["command"], json!(["/bin/oha"]));
        assert_eq!(
            container["env"],
            json!([{"name":"ROUTE_CALLER_PAT","valueFrom":{"secretKeyRef":{"name":"selected-pat","key":"token"}}}])
        );
        let args = container["args"].as_array().unwrap();
        let value_after =
            |key: &str| &args[args.iter().position(|value| value == key).unwrap() + 1];
        assert_eq!(value_after("-c"), "4");
        assert_eq!(value_after("-z"), "10s");
        assert_eq!(value_after("--output-format"), "json");
        assert_eq!(value_after("-m"), "POST");
        assert!(args.contains(&json!("Host: selected.example")));
        assert!(args.contains(&json!("Content-Type: application/json")));
        assert!(args.contains(&json!("Authorization: Bearer $(ROUTE_CALLER_PAT)")));
        assert_eq!(
            serde_json::from_str::<Value>(value_after("-d").as_str().unwrap()).unwrap(),
            json!([{"request_id":"bench","id":"00000000-0000-0000-0000-000000000301"}])
        );
        assert_eq!(
            args.last().unwrap(),
            "http://flow-http.selected-environment.svc.cluster.local/purchase_order/get"
        );
        assert!(!serde_json::to_string(&route).unwrap().contains("password"));

        let no_database = rendered_job("nodb", 64).unwrap();
        let container = &no_database["spec"]["template"]["spec"]["containers"][0];
        assert!(container.get("env").is_none());
        assert_eq!(container["command"], json!(["/bin/oha"]));
        assert_eq!(
            container["image"],
            route["spec"]["template"]["spec"]["containers"][0]["image"]
        );
        assert_eq!(
            container["args"],
            json!([
                "--no-tui",
                "-z",
                "10s",
                "-c",
                "64",
                "--output-format",
                "json",
                "-m",
                "GET",
                "-H",
                "Host: selected.example",
                "http://flow-http.selected-environment.svc.cluster.local/no-such-route"
            ])
        );
        assert!(rendered_job("redis", 4).is_err());
        assert!(rendered_job("route", 0).is_err());
    }

    #[test]
    fn throughput_postgres_job_keeps_the_statement_and_runs_its_output_script() {
        use std::os::unix::fs::PermissionsExt as _;
        let job = rendered_job("pg", 16).unwrap();
        let container = &job["spec"]["template"]["spec"]["containers"][0];
        assert_eq!(
            container["image"],
            "postgres@sha256:7157393f508fd8eb46119937fab39813783fe3e7d4c6316c45c12ce2ea25e61d"
        );
        assert_eq!(container["command"], json!(["/bin/sh", "-ec"]));
        assert_eq!(
            container["env"],
            json!([{"name":"PGPASSWORD","value":"probe"}])
        );
        let script = container["args"][0].as_str().unwrap();
        assert!(script.contains("-c 16 -j 8 -T 10"));
        assert!(script.contains("--sampling-rate 0.05"));
        assert!(script.contains("-h 10.89.0.7 -p 5432 -U postgres -d selected_database"));
        let low = rendered_job("pg", 4).unwrap();
        assert!(
            low["spec"]["template"]["spec"]["containers"][0]["args"][0]
                .as_str()
                .unwrap()
                .contains("-c 4 -j 4 -T 10")
        );

        let directory = wamn_test_infrastructure::scratch::ScratchRoot(std::env::temp_dir().join(
            format!("receiving-throughput-test-{}", uuid::Uuid::new_v4()),
        ));
        fs::create_dir(directory.path()).unwrap();
        let program = directory.path().join("pgbench");
        fs::write(
            &program,
            r#"#!/bin/sh
printf '%s\n' "$@" >"$TEST_DIRECTORY/arguments"
script_file=''
previous=''
for argument in "$@"; do
  if [ "$previous" = -f ]; then script_file=$argument; fi
  previous=$argument
done
test -s "$script_file"
grep -q 'receiving.purchase_order' "$script_file"
grep -q "'00000000-0000-0000-0000-000000000301'::uuid" "$script_file"
echo 'tps = 123.4 (without initial connection time)'
printf '0 1 471 0 1788644700 907135\n0 2 103 0 1788644700 907251\n' >"$TEST_DIRECTORY/pgb.test"
"#,
        )
        .unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        // Only the test's temporary paths differ from the rendered script.
        let script = script
            .replace(
                "/tmp/bench.sql",
                &directory.path().join("bench.sql").display().to_string(),
            )
            .replace(
                "/tmp/pgb",
                &directory.path().join("pgb").display().to_string(),
            );
        let output = std::process::Command::new("sh")
            .args(["-ec", &script])
            .env("TEST_DIRECTORY", directory.path())
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    directory.path().display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = String::from_utf8(output.stdout).unwrap();
        assert_eq!(
            output.lines().collect::<Vec<_>>(),
            [
                "tps = 123.4 (without initial connection time)",
                "===LOGS===",
                "0 1 471 0 1788644700 907135",
                "0 2 103 0 1788644700 907251"
            ]
        );
        let arguments = fs::read_to_string(directory.path().join("arguments")).unwrap();
        assert!(arguments.contains("--sampling-rate\n0.05\n"));
    }

    #[test]
    fn fresh_runs_keep_three_pairs_with_the_second_pair_reversed() {
        assert_eq!(
            fresh_runs("service-secret", "human-secret"),
            [
                ("service", 1, "service-secret"),
                ("human", 1, "human-secret"),
                ("human", 2, "human-secret"),
                ("service", 2, "service-secret"),
                ("service", 3, "service-secret"),
                ("human", 3, "human-secret"),
            ]
        );
    }

    #[test]
    fn overhead_limit_uses_five_sample_median() {
        assert_eq!(overhead_median(&[5.0, 6.0, 7.0, 30.0, 40.0]).unwrap(), 7.0);
        assert_eq!(
            overhead_median(&[10.0, 11.0, 12.0, 13.0, 14.0]).unwrap(),
            12.0
        );
        assert!(overhead_median(&[11.0, 12.0, 12.1, 13.0, 14.0]).is_err());
        assert!(overhead_median(&[1.0, 2.0, 3.0, 4.0]).is_err());
        assert!(overhead_median(&[1.0, 2.0, 3.0, 4.0, f64::NAN]).is_err());
    }

    #[test]
    fn throughput_statement_retains_the_generated_read_and_refuses_missing_substitutions() {
        let generated = include_str!("../../../generated/sql/purchase_order/get.sql");
        let sql = benchmark_sql(generated).unwrap();
        assert!(sql.contains("FROM receiving.purchase_order AS model"));
        assert!(sql.ends_with(&format!("= '{PURCHASE_ORDER_ID}'::uuid;")));
        assert!(
            benchmark_sql(&generated.replace(
                "FROM purchase_order AS model",
                "FROM another_table AS model"
            ))
            .is_err()
        );
        assert!(benchmark_sql(&generated.replace("$1::uuid", "$2::uuid")).is_err());
    }

    #[test]
    fn startup_request_keeps_the_recovery_limit_and_purchase_order_identity() {
        let body = json!([{"request_id":"startup-restart-first","value":{"id":PURCHASE_ORDER_ID}}]);
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
