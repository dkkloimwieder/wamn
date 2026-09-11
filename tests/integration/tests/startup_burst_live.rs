//! Native start bursts on one fresh, real WAMN host using the Receiving fixture.
//!
//! This is a herd of replicas of one production digest. Native compilation is
//! deduplicated; it is not a herd of distinct cold components. The journey's
//! trace reducer separately requires observed overlapping native start handlers.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::net::TcpListener;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, ensure};
use futures_util::StreamExt as _;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::process::{Child, Command};
use tokio::task::JoinSet;
use tokio::time::timeout;
use wamn_control_registry::{Env, Triple};
use wamn_ctl::dev::activation::{HOST_SHUTDOWN_TIMEOUT, WORKLOAD_RPC_TIMEOUT};
use wamn_runtime::registry_credentials::read_registry_credentials;
use wamn_test_infrastructure::event_broker::Credentials;
use wash_runtime::washlet::{OPERATOR_API_PREFIX, rpc_subject, types::v2};

// Existing Receiving host-availability and recovery budgets, not new latency gates.
const STARTUP_BUDGET: Duration = Duration::from_secs(120);
const CONTROL_BUDGET: Duration = Duration::from_secs(5);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Inputs {
    pub(crate) source: String,
    pub(crate) host_binary: PathBuf,
    pub(crate) host_secrets: PathBuf,
    pub(crate) registry_auth: PathBuf,
    pub(crate) workload: PathBuf,
    pub(crate) pat_secret: PathBuf,
    pub(crate) private_dir: PathBuf,
    pub(crate) evidence_dir: PathBuf,
    pub(crate) nats_url: String,
    pub(crate) scheduler_nats_url: String,
    pub(crate) otlp_endpoint: String,
    pub(crate) proof_id: String,
    pub(crate) component_artifact_base: String,
    pub(crate) release_artifact_base: String,
    pub(crate) manifest_digest: String,
    pub(crate) org: String,
    pub(crate) project: String,
    pub(crate) schema: String,
    pub(crate) environment: String,
    pub(crate) route_host: String,
    pub(crate) route_path: String,
    pub(crate) probe_body: Value,
    pub(crate) max_concurrent_starts: usize,
}

fn read_json(path: &Path) -> Result<Value> {
    serde_json::from_slice(&std::fs::read(path)?).context("read private fixture JSON")
}

fn credential(root: &Path, name: &str) -> Result<String> {
    read_json(&root.join(format!("{name}.json")))?["stringData"]["url"]
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .context("missing provisioned authority credential")
}

async fn rpc<Q: serde::Serialize, A: DeserializeOwned>(
    client: &async_nats::Client,
    host: &str,
    command: &str,
    request: &Q,
    budget: Duration,
) -> Result<A> {
    let payload = serde_json::to_vec(request)?;
    let response = timeout(
        budget,
        client.request(rpc_subject(host, command), payload.into()),
    )
    .await
    .context("native request exceeded its existing budget")?
    .map_err(|_| anyhow::anyhow!("native request failed"))?;
    serde_json::from_slice(&response.payload).context("decode native response")
}

fn workload_request(inputs: &Inputs, id: &str) -> Result<v2::WorkloadStartRequest> {
    let actual = read_json(&inputs.workload)?;
    let spec = &actual["spec"];
    let components = spec["components"]
        .as_array()
        .context("production components")?;
    ensure!(
        components.len() == 1,
        "expected the single production HTTP shell"
    );
    let component = &components[0];
    let image = component["image"].as_str().context("production image")?;
    let registry = image.split('/').next().context("image authority")?;
    let credentials = read_registry_credentials(&inputs.registry_auth, registry)?;
    let interfaces: Vec<v2::WitInterface> = serde_json::from_value(spec["hostInterfaces"].clone())?;
    ensure!(
        interfaces
            .iter()
            .any(|interface| interface.namespace == "wasi"
                && interface.package == "http"
                && interface.config.get("host") == Some(&inputs.route_host)),
        "production HTTP authority differs from the request fixture"
    );
    Ok(v2::WorkloadStartRequest {
        workload_id: id.to_owned(),
        workload: Some(v2::Workload {
            namespace: inputs.environment.clone(),
            name: id.to_owned(),
            wit_world: Some(v2::WitWorld {
                components: vec![v2::Component {
                    name: component["name"]
                        .as_str()
                        .context("component name")?
                        .to_owned(),
                    image: image.to_owned(),
                    image_pull_policy: v2::ImagePullPolicy::Always.into(),
                    image_pull_secret: Some(v2::ImagePullSecret {
                        username: credentials.username().to_owned(),
                        password: credentials.password().to_owned(),
                    }),
                    local_resources: Some(serde_json::from_value(
                        component["localResources"].clone(),
                    )?),
                    ..Default::default()
                }],
                host_interfaces: interfaces,
            }),
            ..Default::default()
        }),
    })
}

async fn application_request(
    inputs: &Inputs,
    http: &reqwest::Client,
    base: &str,
    token: &str,
    id: &str,
) -> Result<Value> {
    let started = Instant::now();
    let mut body = inputs.probe_body.clone();
    body["request_id"] = json!(id);
    let response = http
        .post(format!("{base}{}", inputs.route_path))
        .header("Host", &inputs.route_host)
        .header("Content-Type", "application/json")
        .bearer_auth(token)
        .body(serde_json::to_vec(&vec![body])?)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("application request failed"))?;
    let status = response.status().as_u16();
    let bytes = response.bytes().await?;
    if status == 200 {
        let value: Value = serde_json::from_slice(&bytes)?;
        ensure!(
            value.as_array().is_some_and(|items| items.len() == 1)
                && value[0]["request_id"] == id
                && value[0].get("error").is_none()
                && value[0]["value"]["id"] == inputs.probe_body["id"],
            "the production read did not return the requested Receiving record"
        );
    } else {
        ensure!(
            matches!(status, 404 | 503) && bytes.is_empty(),
            "initial refusal was not an empty native 404 or 503"
        );
    }
    Ok(json!({"status":status,"body_hex":hex::encode(&bytes),
        "elapsed_seconds":started.elapsed().as_secs_f64()}))
}

#[expect(
    clippy::too_many_arguments,
    reason = "One private journey operation receives the actual fixture and the two native transports."
)]
async fn burst(
    inputs: &Inputs,
    client: &async_nats::Client,
    host: &str,
    http: &reqwest::Client,
    base: &str,
    probe: &str,
    token: &str,
    phase: &str,
    owned: &mut Vec<String>,
    receipt: &mut Value,
) -> Result<()> {
    let began = Instant::now();
    receipt["started_unix_ns"] = json!(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_nanos()
            .to_string()
    );
    // Two native windows create demand above the configured setting; exposure
    // still requires server span overlap, verified by the journey's reducer.
    let count = inputs
        .max_concurrent_starts
        .checked_mul(2)
        .context("burst count")?;
    let mut starts = JoinSet::new();
    for index in 0..count {
        let id = format!("{}-{phase}-{index}", inputs.proof_id);
        let request = workload_request(inputs, &id)?;
        owned.push(id.clone());
        let client = client.clone();
        let host = host.to_owned();
        starts.spawn(async move {
            let start = began.elapsed().as_secs_f64();
            let response: v2::WorkloadStartResponse = rpc(
                &client,
                &host,
                "workload.start",
                &request,
                WORKLOAD_RPC_TIMEOUT,
            )
            .await?;
            let status = response.workload_status.context("start response status")?;
            ensure!(
                status.workload_id == id && status.workload_state() == v2::WorkloadState::Running,
                "production workload start did not reach Running"
            );
            Ok::<_, anyhow::Error>(json!({"id":id,"request_started_seconds":start,
                "response_seconds":began.elapsed().as_secs_f64(),"state":"Running"}))
        });
    }
    let mut timer = tokio::time::interval(Duration::from_millis(50));
    let mut first_success = None;
    while !starts.is_empty() {
        tokio::select! {
            response = starts.join_next() => {
                receipt["starts"].as_array_mut().unwrap().push(
                    response.context("start task disappeared")???,
                );
            }
            _ = timer.tick() => {
                let observation_started = began.elapsed().as_secs_f64();
                let heartbeat: v2::HostHeartbeat = rpc(client, host, "heartbeat",
                    &v2::HostHeartbeatRequest::default(), CONTROL_BUDGET).await?;
                ensure!(heartbeat.id == host, "control reply belongs to another host");
                let live = http.get(format!("{probe}/livez")).send().await?.status().as_u16();
                let ready = http.get(format!("{probe}/readyz")).send().await?.status().as_u16();
                ensure!(live == 200 && ready == 200, "native probes failed during starts");
                let request_id = format!("{}-{phase}-{}", inputs.proof_id,
                    receipt["observations"].as_array().unwrap().len());
                let response = application_request(inputs, http, base, token, &request_id).await?;
                let status = response["status"].as_u64().unwrap();
                if phase == "warm" || first_success.is_some() {
                    ensure!(status == 200, "an already serving route failed during the burst");
                } else {
                    // Explicit residual startup statuses; no mutation is retried.
                    ensure!([200, 404, 503].contains(&status), "unexpected initial route refusal");
                }
                if status == 200 && first_success.is_none() {
                    first_success = Some(began.elapsed().as_secs_f64());
                }
                receipt["observations"].as_array_mut().unwrap().push(json!({
                    "started_seconds":observation_started,
                    "finished_seconds":began.elapsed().as_secs_f64(),
                    "pending_client_starts":starts.len(),"heartbeat_workload_count":heartbeat.workload_count,
                    "native_live_status":live,"native_ready_status":ready,"application":response}));
            }
        }
    }
    receipt["all_running_seconds"] = json!(began.elapsed().as_secs_f64());
    let after = application_request(
        inputs,
        http,
        base,
        token,
        &format!("{}-{phase}-final", inputs.proof_id),
    )
    .await?;
    ensure!(
        after["status"] == 200,
        "route did not serve after all starts"
    );
    let completed = began.elapsed().as_secs_f64();
    receipt["first_success_seconds"] = json!(first_success.unwrap_or(completed));
    receipt["final_success_seconds"] = json!(completed);
    receipt["final_application"] = after;
    Ok(())
}

async fn stop_child(child: &mut Child) -> Result<Value> {
    let pid = child.id();
    let began = Instant::now();
    if let Some(pid) = pid {
        Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .await?;
    }
    if let Ok(status) = timeout(HOST_SHUTDOWN_TIMEOUT, child.wait()).await {
        let status = status?;
        ensure!(status.success(), "proof host exited unsuccessfully");
        Ok(json!({"pid":pid,"exit_code":status.code(),"forced":false,
            "seconds":began.elapsed().as_secs_f64()}))
    } else {
        child.start_kill()?;
        timeout(CONTROL_BUDGET, child.wait()).await??;
        anyhow::bail!("proof host exceeded the existing host shutdown budget")
    }
}

#[tokio::test]
#[ignore = "requires the private completed Receiving fixture and rebuilt production host"]
async fn production_http_start_burst_keeps_native_host_progress() -> Result<()> {
    let input_path = std::env::var_os("WAMN_STARTUP_BURST_INPUT")
        .context("WAMN_STARTUP_BURST_INPUT must name the runner-owned fixture")?;
    let inputs: Inputs = serde_json::from_slice(&std::fs::read(PathBuf::from(input_path))?)?;
    let credentials = Credentials {
        username: std::env::var("WAMN_EVT_NATS_USERNAME")?,
        password_file: PathBuf::from(
            std::env::var_os("WAMN_EVT_NATS_PASSWORD_FILE")
                .context("the startup test requires its private NATS password file")?,
        ),
    };
    let scope = Triple {
        org: std::env::var("WAMN_EVT_ORG")?,
        project: std::env::var("WAMN_EVT_PROJECT")?,
        env: Env::new(std::env::var("WAMN_EVT_ENV")?),
    };
    assert_startup(&inputs, &credentials, &scope).await
}

pub(crate) async fn assert_startup(
    inputs: &Inputs,
    credentials: &Credentials,
    scope: &Triple,
) -> Result<()> {
    ensure!(
        inputs.max_concurrent_starts > 0,
        "native start limit must be nonzero"
    );
    ensure!(
        inputs.scheduler_nats_url != inputs.nats_url,
        "the startup proof requires separate scheduler and event brokers"
    );
    let mut receipt = json!({"source":inputs.source,"proof_id":inputs.proof_id,
        "native_start_limit":inputs.max_concurrent_starts,"profile":"release",
        "manifest_digest":inputs.manifest_digest,"verdict":"fail",
        "cache_scope":"Fresh host/empty private caches; native HTTP digest is first loaded by cold herd; replicas share native compile deduplication.",
        "control_scope":"Native washlet RPC on the owned chart scheduler Service, with a unique proof host/group and exact host heartbeat filtering; event NATS is separate.",
        "phase_attribution":"Whole native starts and observed progress only. Retired private Wasm/non-Wasm phase attribution unavailable.",
        "cache_precondition":"Both private OCI and Wasmtime cache directories were created empty. Release preload may compile release callables before readiness; the HTTP shell is not one of those callables.",
        "cold":{"starts":[],"observations":[]},"warm":{"starts":[],"observations":[]}});
    let client = timeout(
        CONTROL_BUDGET,
        async_nats::ConnectOptions::new()
            .request_timeout(None)
            .connect(&inputs.scheduler_nats_url),
    )
    .await??;
    let mut heartbeats = client
        .subscribe(format!("{OPERATOR_API_PREFIX}.heartbeat.*"))
        .await?;
    client.flush().await?;
    let cache = inputs.private_dir.join("wasmtime-cache");
    let oci_cache = inputs.private_dir.join("oci-cache");
    std::fs::create_dir(&cache)?;
    std::fs::create_dir(&oci_cache)?;
    ensure!(
        std::fs::read_dir(&cache)?.next().is_none()
            && std::fs::read_dir(&oci_cache)?.next().is_none(),
        "new proof caches are not empty"
    );
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let probe_address = listener.local_addr()?;
    drop(listener);
    let raw_log = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(inputs.private_dir.join("host.raw.log"))?;
    let mut command = Command::new(&inputs.host_binary);
    command
        .env_clear()
        .args([
            "host",
            "--host-group",
            &inputs.proof_id,
            "--host-name",
            &inputs.proof_id,
            "--runner",
            &inputs.proof_id,
            "--environment",
            &inputs.environment,
            "--scheduler-nats-url",
            &inputs.scheduler_nats_url,
            "--http-addr",
            "127.0.0.1:0",
            "--probe-addr",
            &probe_address.to_string(),
            "--max-concurrent-starts",
            &inputs.max_concurrent_starts.to_string(),
            "--release-artifact-base",
            &inputs.release_artifact_base,
            "--release-manifest-digest",
            &inputs.manifest_digest,
            "--component-artifact-base",
            &inputs.component_artifact_base,
            "--project",
            &inputs.project,
            "--org",
            &inputs.org,
            "--schema",
            &inputs.schema,
            "--allow-insecure-registries",
        ])
        .arg("--registry-auth-file")
        .arg(&inputs.registry_auth)
        .arg("--wasmtime-cache-dir")
        .arg(&cache)
        .arg("--oci-cache-dir")
        .arg(&oci_cache)
        .env("WAMN_EVT_NATS_URL", &inputs.nats_url)
        .env("OTEL_EXPORTER_OTLP_ENDPOINT", &inputs.otlp_endpoint)
        .env("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc")
        .env(
            "OTEL_RESOURCE_ATTRIBUTES",
            format!("wamn.startup.proof={}", inputs.proof_id),
        )
        .env("OTEL_BSP_SCHEDULE_DELAY", "1")
        .env("OTEL_BSP_MAX_EXPORT_BATCH_SIZE", "1")
        .env("WASH_HOST_MAX_GUEST_MEMORY", "4Gi")
        .env("WASH_DEFAULT_HEAP_MEMORY", "256MiB")
        .env("WASH_CORE_INSTANCES", "512")
        .env("WASH_GUEST_MEMORY_MODE", "count")
        .env("WAMN_PG_GUEST_POOL_MAX", "14")
        .env("WAMN_PG_PLATFORM_POOL_MAX", "2")
        .env("WAMN_HTTP_ROUTE_IN_FLIGHT_LIMIT", "64")
        .stdout(Stdio::from(raw_log.try_clone()?))
        .stderr(Stdio::from(raw_log))
        .kill_on_drop(true);
    command
        .env("WAMN_EVT_NATS_USERNAME", &credentials.username)
        .env("WAMN_EVT_NATS_PASSWORD_FILE", &credentials.password_file)
        .env("WAMN_EVT_ORG", &scope.org)
        .env("WAMN_EVT_PROJECT", &scope.project)
        .env("WAMN_EVT_ENV", scope.env.as_str());
    for (key, name) in [
        ("WAMN_SYSTEM_URL", "identity-reader"),
        ("WAMN_PG_URL", "guest-sql"),
        ("WAMN_EXECUTOR_PLATFORM_PG_URL", "executor-platform"),
        ("WAMN_HTTP_ADMITTER_PG_URL", "http-admitter"),
        ("WAMN_EVENT_MATERIALIZER_PG_URL", "event-materializer"),
    ] {
        command.env(key, credential(&inputs.host_secrets, name)?);
    }
    let began = Instant::now();
    receipt["process_started_unix_ns"] = json!(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_nanos()
            .to_string()
    );
    let mut child = command
        .spawn()
        .context("start fresh production WAMN host")?;
    receipt["pid"] = json!(child.id());
    let mut owned = Vec::new();
    let mut host_id = None;
    let outcome = async {
        let heartbeat = timeout(STARTUP_BUDGET, async {
            while let Some(message) = heartbeats.next().await {
                let heartbeat: v2::HostHeartbeat = serde_json::from_slice(&message.payload)?;
                if heartbeat.hostname == inputs.proof_id
                    && heartbeat.environment == inputs.environment
                {
                    return Ok::<_, anyhow::Error>(heartbeat);
                }
            }
            anyhow::bail!("heartbeat subscription closed")
        })
        .await??;
        host_id = Some(heartbeat.id.clone());
        ensure!(
            heartbeat.workload_count == 0,
            "fresh host already has native workloads"
        );
        receipt["host"] = serde_json::to_value(&heartbeat)?;
        receipt["heartbeat_seconds"] = json!(began.elapsed().as_secs_f64());
        let http = reqwest::Client::builder().timeout(CONTROL_BUDGET).build()?;
        let base = format!("http://127.0.0.1:{}", heartbeat.http_port);
        let probe = format!("http://{probe_address}");
        timeout(STARTUP_BUDGET, async {
            loop {
                if let Ok(response) = http.get(format!("{probe}/readyz")).send().await
                    && response.status() == 200
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .context("fresh host native readiness exceeded existing budget")?;
        receipt["host_ready_seconds"] = json!(began.elapsed().as_secs_f64());
        let pat = read_json(&inputs.pat_secret)?;
        let token = pat["stringData"]["token"]
            .as_str()
            .context("private route PAT")?;
        for phase in ["cold", "warm"] {
            burst(
                &inputs,
                &client,
                &heartbeat.id,
                &http,
                &base,
                &probe,
                token,
                phase,
                &mut owned,
                &mut receipt[phase],
            )
            .await?;
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let mut cleanup_errors = 0;
    if let Some(host) = host_id {
        for id in &owned {
            let stopped: Result<v2::WorkloadStopResponse> = rpc(
                &client,
                &host,
                "workload.stop",
                &v2::WorkloadStopRequest {
                    workload_id: id.clone(),
                },
                WORKLOAD_RPC_TIMEOUT,
            )
            .await;
            if stopped.is_err() {
                cleanup_errors += 1;
            }
        }
        let empty: Result<v2::HostHeartbeat> = rpc(
            &client,
            &host,
            "heartbeat",
            &v2::HostHeartbeatRequest::default(),
            CONTROL_BUDGET,
        )
        .await;
        if !empty.is_ok_and(|heartbeat| heartbeat.workload_count == 0) {
            cleanup_errors += 1;
        }
    }
    let stopped = stop_child(&mut child).await;
    receipt["fixture_cgroup"] = json!(std::fs::read_to_string("/proc/self/cgroup").ok());
    receipt["fixture_load_average"] = json!(std::fs::read_to_string("/proc/loadavg").ok());
    receipt["host_shutdown"] = match &stopped {
        Ok(value) => value.clone(),
        Err(_) => json!({"verdict":"fail"}),
    };
    receipt["owned_workloads"] = json!(owned);
    receipt["cleanup_errors"] = json!(cleanup_errors);
    receipt["verdict"] = json!(
        if outcome.is_ok() && stopped.is_ok() && cleanup_errors == 0 {
            "protocol-pass-awaiting-trace-exposure"
        } else {
            "fail"
        }
    );
    // Keep transport error context private until the supervisor redacts it.
    if outcome.is_err() || stopped.is_err() {
        let mut diagnostics = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(inputs.private_dir.join("failure.raw.log"))?;
        if let Err(error) = &outcome {
            writeln!(diagnostics, "startup: {error:#}")?;
        }
        if let Err(error) = &stopped {
            writeln!(diagnostics, "shutdown: {error:#}")?;
        }
    }
    std::fs::write(
        inputs.evidence_dir.join("protocol.json"),
        serde_json::to_vec_pretty(&receipt)?,
    )?;
    ensure!(
        outcome.is_ok(),
        "startup burst failed; inspect retained observations and redacted host log"
    );
    ensure!(
        stopped.is_ok() && cleanup_errors == 0,
        "startup burst cleanup failed"
    );
    Ok(())
}
