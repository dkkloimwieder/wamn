//! Lifecycle test for the rebuilt host and an owned real NATS server.
//!
//! The ignored test requires an absolute NATS server binary and a new evidence
//! directory. It starts no PostgreSQL, OCI registry, operator, or guest workload.
//! Missing-first-beat failure uses native objects and WAMN helpers in this test
//! process; the signal and exporter cases exercise the rebuilt host child.

use std::fs::{self, File};
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::time::{Instant, sleep, timeout};

const REQUEST_BUDGET: Duration = Duration::from_secs(1);
const STARTUP_BUDGET: Duration = Duration::from_secs(60);
const SHUTDOWN_BUDGET: Duration = Duration::from_secs(70);
const REAP_BUDGET: Duration = Duration::from_secs(5);
// Exceeds the native 3 × 15-second silence budget without changing that budget.
const NATS_OUTAGE: Duration = Duration::from_secs(50);

fn reserve_address() -> anyhow::Result<TcpListener> {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).context("reserve a loopback port")
}

fn logs(command: &mut Command, evidence: &Path, name: &str) -> anyhow::Result<()> {
    command
        .stdin(Stdio::null())
        .stdout(File::create(evidence.join(format!("{name}.stdout")))?)
        .stderr(File::create(evidence.join(format!("{name}.stderr")))?)
        .kill_on_drop(true);
    Ok(())
}

async fn start_nats(
    binary: &Path,
    address: SocketAddr,
    evidence: &Path,
    name: &str,
) -> anyhow::Result<Child> {
    let mut command = Command::new(binary);
    command
        .env_clear()
        .arg("--addr")
        .arg(address.ip().to_string())
        .arg("--port")
        .arg(address.port().to_string());
    logs(&mut command, evidence, name)?;
    let mut child = command.spawn().context("start the owned NATS server")?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        ensure!(
            child.try_wait()?.is_none(),
            "owned NATS exited during startup"
        );
        if let Ok(Ok(client)) =
            timeout(REQUEST_BUDGET, async_nats::connect(address.to_string())).await
        {
            timeout(REQUEST_BUDGET, client.flush()).await??;
            return Ok(child);
        }
        ensure!(Instant::now() < deadline, "owned NATS did not become ready");
        sleep(Duration::from_millis(50)).await;
    }
}

async fn probe(address: SocketAddr, path: &str) -> anyhow::Result<(u16, String)> {
    timeout(REQUEST_BUDGET, async {
        let mut stream = TcpStream::connect(address).await?;
        stream
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await?;
        let mut raw = String::new();
        stream.take(4096).read_to_string(&mut raw).await?;
        let (head, body) = raw
            .split_once("\r\n\r\n")
            .context("HTTP response has no body boundary")?;
        let status = head
            .split_whitespace()
            .nth(1)
            .context("HTTP response has no status")?
            .parse()?;
        Ok((status, body.to_owned()))
    })
    .await
    .context("native probe exceeded one second")?
}

async fn expect_probe(
    address: SocketAddr,
    path: &str,
    status: u16,
    body: &str,
) -> anyhow::Result<()> {
    let actual = probe(address, path).await?;
    ensure!(
        actual == (status, body.to_owned()),
        "unexpected {path} response: {actual:?}"
    );
    Ok(())
}

async fn await_ready(host: &mut Child, address: SocketAddr) -> anyhow::Result<bool> {
    let deadline = Instant::now() + STARTUP_BUDGET;
    let mut observed_starting = false;
    loop {
        ensure!(host.try_wait()?.is_none(), "host exited during startup");
        if let Ok((status, body)) = probe(address, "/livez").await {
            ensure!(
                status == 200 && body == "ok\n",
                "startup liveness failed: {status} {body:?}"
            );
            let (status, body) = probe(address, "/readyz").await?;
            match (status, body.as_str()) {
                (200, "ok\n") => return Ok(observed_starting),
                (503, "starting\n") => observed_starting = true,
                _ => anyhow::bail!("unexpected startup readiness: {status} {body:?}"),
            }
        }
        ensure!(
            Instant::now() < deadline,
            "host did not become ready before its startup deadline"
        );
        sleep(Duration::from_millis(20)).await;
    }
}

async fn await_readiness(
    host: &mut Child,
    address: SocketAddr,
    status: u16,
    body: &str,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        ensure!(
            host.try_wait()?.is_none(),
            "host exited before the readiness transition"
        );
        if probe(address, "/readyz").await? == (status, body.to_owned()) {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "readiness did not become {status} {body:?}"
        );
        sleep(Duration::from_millis(20)).await;
    }
}

fn native_host_id(evidence: &Path, name: &str) -> anyhow::Result<String> {
    // Native Host started diagnostics identify the private RPC subject. Do not
    // derive it from --host-name: native host identity is a separate value.
    for suffix in ["stdout", "stderr"] {
        let log = fs::read_to_string(evidence.join(format!("{name}.{suffix}")))?;
        for line in log.lines().filter(|line| line.contains("Host started")) {
            if let Some((_, value)) = line.split_once("host_id=") {
                let value = value.trim_start_matches('"');
                let id = value
                    .split(|c: char| c == '"' || c.is_whitespace())
                    .next()
                    .unwrap_or_default();
                if !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                    return Ok(id.to_owned());
                }
            }
        }
    }
    anyhow::bail!("native Host started log did not identify the host RPC subject")
}

async fn heartbeat_rpc(
    address: SocketAddr,
    host_id: &str,
    host_name: &str,
) -> anyhow::Result<Vec<u8>> {
    timeout(Duration::from_secs(30), async {
        let client = async_nats::connect(address.to_string()).await?;
        let subject = wash_runtime::washlet::rpc_subject(host_id, "heartbeat");
        loop {
            // Server readiness precedes the host client's reconnect/resubscribe.
            if let Ok(Ok(response)) = timeout(
                REQUEST_BUDGET,
                client.request(subject.clone(), Vec::new().into()),
            )
            .await
            {
                let body = std::str::from_utf8(&response.payload)?;
                ensure!(
                    body.contains(host_name),
                    "native heartbeat RPC did not name this host"
                );
                return Ok(response.payload.to_vec());
            }
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .context("native heartbeat RPC did not recover before its deadline")?
}

async fn send_signal(child: &Child, signal: &str) -> anyhow::Result<()> {
    let pid = child.id().context("host has no live process ID")?;
    let output = timeout(
        REAP_BUDGET,
        Command::new("/bin/kill")
            .args(["-s", signal, &pid.to_string()])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("signal delivery command exceeded its deadline")??;
    ensure!(output.status.success(), "signal delivery command failed");
    Ok(())
}

fn host_command(
    nats_address: SocketAddr,
    ingress_address: SocketAddr,
    probe_address: SocketAddr,
    evidence: &Path,
    name: &str,
) -> anyhow::Result<Command> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_wamn-host"));
    command.env_clear().args([
        "host",
        "--host-name",
        name,
        "--host-group",
        "lifecycle-test",
        "--environment",
        "lifecycle-test",
        "--scheduler-nats-url",
        &format!("nats://{nats_address}"),
        "--http-addr",
        &ingress_address.to_string(),
        "--probe-addr",
        &probe_address.to_string(),
        "--max-http-ingress-connections",
        "1",
        "--max-concurrent-starts",
        "1",
        "--guest-memory-mode",
        "count",
        "--drain-delay",
        "5s",
    ]);
    logs(&mut command, evidence, name)?;
    Ok(command)
}

async fn assert_host_lifecycle(
    nats: &mut Child,
    nats_binary: &Path,
    nats_address: SocketAddr,
    evidence: &Path,
    signal: &str,
) -> anyhow::Result<()> {
    let ingress_port = reserve_address()?;
    let probe_port = reserve_address()?;
    let ingress_address = ingress_port.local_addr()?;
    let probe_address = probe_port.local_addr()?;
    let name = format!("host-{}", signal.to_ascii_lowercase());
    let mut command = host_command(
        nats_address,
        ingress_address,
        probe_address,
        evidence,
        &name,
    )?;
    drop((ingress_port, probe_port));
    let mut host = command
        .spawn()
        .context("start the rebuilt wamn-host binary")?;
    let result = async {
        let observed_starting = await_ready(&mut host, probe_address).await?;
        let host_id = native_host_id(evidence, &name)?;
        fs::write(evidence.join(format!("{name}-heartbeat-before.json")),
            heartbeat_rpc(nats_address, &host_id, &name).await?)?;

        let held = TcpStream::connect(ingress_address).await?;
        await_readiness(&mut host, probe_address, 503, "http_ingress_saturated\n").await?;
        for _ in 0..3 {
            expect_probe(probe_address, "/livez", 200, "ok\n").await?;
            expect_probe(probe_address, "/readyz", 503, "http_ingress_saturated\n").await?;
        }
        drop(held);
        await_readiness(&mut host, probe_address, 200, "ok\n").await?;

        if signal == "TERM" {
            timeout(REAP_BUDGET, nats.kill()).await.context("stop and reap owned NATS")??;
            let disconnected = Instant::now();
            while disconnected.elapsed() < NATS_OUTAGE {
                ensure!(host.try_wait()?.is_none(), "host exited while scheduler NATS was unavailable");
                expect_probe(probe_address, "/livez", 200, "ok\n").await?;
                expect_probe(probe_address, "/readyz", 200, "ok\n").await?;
                sleep(Duration::from_millis(250)).await;
            }
            *nats = start_nats(nats_binary, nats_address, evidence, "nats-restarted").await?;
            fs::write(evidence.join(format!("{name}-heartbeat-after.json")),
                heartbeat_rpc(nats_address, &host_id, &name).await?)?;
        }

        let signalled = Instant::now();
        send_signal(&host, signal).await?;
        await_readiness(&mut host, probe_address, 503, "draining\n").await?;
        expect_probe(probe_address, "/livez", 200, "ok\n").await?;
        let remaining = SHUTDOWN_BUDGET.saturating_sub(signalled.elapsed());
        let status = timeout(remaining, host.wait()).await.context("host exceeded 70-second signal-to-exit budget")??;
        ensure!(status.success(), "host did not exit successfully after SIG{signal}: {status}");
        fs::write(evidence.join(format!("{name}.receipt")), format!(
            "signal=SIG{signal}\nexit_success=true\nshutdown_ms={}\nshutdown_budget_ms={}\nstartup_starting_observed={observed_starting}\nsaturation_probe_pass=true\nrecovery_probe_pass=true\nnats_outage_seconds={}\n",
            signalled.elapsed().as_millis(), SHUTDOWN_BUDGET.as_millis(),
            if signal == "TERM" { NATS_OUTAGE.as_secs() } else { 0 },
        ))?;
        Ok(())
    }.await;
    // Keep an assertion failure from leaving the actual host behind.
    if host.try_wait()?.is_none() {
        timeout(REAP_BUDGET, host.kill())
            .await
            .context("kill and reap failed test host")??;
    }
    result
}

async fn assert_blocked_flush(nats_address: SocketAddr, evidence: &Path) -> anyhow::Result<()> {
    let ingress_port = reserve_address()?;
    let probe_port = reserve_address()?;
    let ingress_address = ingress_port.local_addr()?;
    let probe_address = probe_port.local_addr()?;
    let trace_peer = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    // Logs and metrics also stay on an owned loopback listener. The distinct
    // trace endpoint makes the observed connection unambiguously a trace export.
    let other_peer = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let name = "host-blocked-flush";
    let mut command = host_command(nats_address, ingress_address, probe_address, evidence, name)?;
    command
        .env("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc")
        .env(
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            format!("http://{}", other_peer.local_addr()?),
        )
        .env(
            "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
            format!("http://{}", trace_peer.local_addr()?),
        )
        .env("OTEL_EXPORTER_OTLP_TIMEOUT", "60000")
        .env("OTEL_EXPORTER_OTLP_TRACES_TIMEOUT", "60000")
        .env("OTEL_BSP_SCHEDULE_DELAY", "1")
        .env("OTEL_BSP_MAX_EXPORT_BATCH_SIZE", "1")
        .env("OTEL_TRACES_SAMPLER", "always_on");
    drop((ingress_port, probe_port));
    let mut host = command
        .spawn()
        .context("start host with the owned stalled trace exporter")?;
    let result = async {
        await_ready(&mut host, probe_address).await?;
        // Native ingress instruments this path before its no-workload refusal.
        let (status, _) = probe(ingress_address, "/flush-test").await?;
        ensure!(status == 404, "release-less native ingress did not complete its expected refusal");
        let (mut connection, _) = timeout(Duration::from_secs(10), trace_peer.accept())
            .await.context("the real trace exporter made no connection")??;
        let mut preface = [0u8; 24];
        timeout(REQUEST_BUDGET, connection.read_exact(&mut preface))
            .await.context("trace exporter sent no HTTP/2 preface")??;
        ensure!(&preface == b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n", "trace connection was not native OTLP gRPC");
        fs::write(evidence.join("trace-peer-preface.bin"), preface)?;
        // Keep the connection open without answering until the actual host exits.
        let signalled = Instant::now();
        send_signal(&host, "TERM").await?;
        await_readiness(&mut host, probe_address, 503, "draining\n").await?;
        expect_probe(probe_address, "/livez", 200, "ok\n").await?;
        let remaining = SHUTDOWN_BUDGET.saturating_sub(signalled.elapsed());
        let status = timeout(remaining, host.wait()).await
            .context("blocked exporter kept host alive beyond its shutdown budget")??;
        ensure!(status.code().is_some_and(|code| code != 0), "blocked flush did not return a process error: {status}");
        let stderr = fs::read_to_string(evidence.join(format!("{name}.stderr")))?;
        ensure!(stderr.contains("telemetry flush did not complete"), "nonzero exit did not identify the bounded flush failure");
        fs::write(evidence.join(format!("{name}.receipt")), format!(
            "signal=SIGTERM\nexit_code={}\nshutdown_ms={}\ntrace_export_connection_observed=true\ntrace_peer_answered=false\nflush_failure_reported=true\n",
            status.code().context("failed host has no exit code")?, signalled.elapsed().as_millis(),
        ))?;
        drop(connection);
        Ok(())
    }.await;
    if host.try_wait()?.is_none() {
        timeout(REAP_BUDGET, host.kill())
            .await
            .context("kill and reap blocked-flush test host")??;
    }
    result
}

// Exercise a real native command-task failure through the public WAMN
// lifecycle helpers. This case runs in the test process, not the host child.
async fn assert_missing_first_beat(nats_address: SocketAddr, evidence: &Path) -> anyhow::Result<()> {
    use std::sync::Arc;

    use wamn_runtime::lifecycle::{bounded_cleanup, watch_liveness};
    use wash_runtime::host::probes::{Liveness, ProbeState};
    use wash_runtime::washlet::{ClusterHostBuilder, liveness_silence};

    let client = timeout(
        REQUEST_BUDGET,
        async_nats::connect(nats_address.to_string()),
    )
    .await
    .context("connect the first-beat test client within its budget")??;
    timeout(REQUEST_BUDGET, client.flush())
        .await
        .context("flush the connected first-beat test client")??;
    timeout(REQUEST_BUDGET, client.drain())
        .await
        .context("request the owned client drain")??;
    // drain() only queues closure. Observe a failed valid subscription before
    // giving this client to the native host, so the first-beat failure is real.
    let closed = timeout(REQUEST_BUDGET, async {
        loop {
            match client.subscribe("lifecycle.first-beat.closed").await {
                Ok(subscription) => drop(subscription),
                Err(error) => break error,
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("drained client still accepted subscription commands")?;
    ensure!(
        closed.kind() == async_nats::SubscribeErrorKind::Other,
        "the closed-client control failed for a reason other than command delivery"
    );

    // A short public heartbeat interval keeps this test bounded without a
    // zero silence budget or synthetic beat. The deployment default is 15s.
    let builder = ClusterHostBuilder::default()
        .with_host_group("lifecycle-test")
        .with_nats_client(Arc::new(client))
        .with_heartbeat_interval(Duration::from_millis(100));
    let silence_budget = liveness_silence(builder.heartbeat_interval());
    let liveness = Liveness::new(silence_budget);
    let probes = ProbeState::default().with_liveness(Arc::clone(&liveness));
    // No ingress, plugins or workloads: a subscribe failure skips Host::stop.
    let native = builder.with_liveness(Arc::clone(&liveness)).build()?;
    let (host, cleanup) = timeout(REQUEST_BUDGET, native.start())
        .await
        .context("native start did not return before its first-beat test budget")??;
    let started = Instant::now();
    let observed = timeout(
        silence_budget + REQUEST_BUDGET,
        watch_liveness(&liveness, &probes, silence_budget),
    )
    .await;
    let observation_time = started.elapsed();
    let never_beat = liveness.silence().is_none();
    // Poll cleanup only after the observer settles, even on its timeout path.
    probes.drain();
    let cleanup_started = Instant::now();
    let cleaned = bounded_cleanup(REQUEST_BUDGET, cleanup).await;
    let cleanup_time = cleanup_started.elapsed();
    drop(host);

    let error =
        observed.context("the failed native task was not detected within its silence bound")?;
    ensure!(
        error.to_string().contains("first liveness beat") && never_beat,
        "the native task did not fail before its first beat: {error:#}"
    );
    ensure!(
        observation_time >= silence_budget && observation_time < silence_budget + REQUEST_BUDGET,
        "first-beat observation did not preserve its configured silence bound"
    );
    ensure!(
        cleanup_time < REQUEST_BUDGET,
        "native cleanup exceeded its budget"
    );
    let error = cleaned
        .err()
        .context("native subscription failure returned successful cleanup")?;
    let subscription = error
        .downcast_ref::<async_nats::SubscribeError>()
        .context("cleanup did not retrieve the native subscription error")?;
    ensure!(
        subscription.kind() == async_nats::SubscribeErrorKind::Other
            && error.to_string() == "failed to subscribe for API requests",
        "cleanup returned a different native failure: {error:#}"
    );
    fs::write(
        evidence.join("native-missing-first-beat.receipt"),
        format!(
            "scope=native ClusterHost and WAMN lifecycle helpers in test process\nclosed_client_subscription_failed=true\nheartbeat_interval_ms=100\nsilence_budget_ms={}\nobservation_ms={}\nnever_beat=true\ncleanup_polled_after_observer=true\nnative_subscription_error_retrieved=true\ncleanup_ms={}\ncleanup_budget_ms={}\nfull_process_unexpected_failure_proved=false\n",
            silence_budget.as_millis(),
            observation_time.as_millis(),
            cleanup_time.as_millis(),
            REQUEST_BUDGET.as_millis(),
        ),
    )?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an owned real nats-server binary and a fresh external evidence directory"]
async fn rebuilt_host_probes_signals_and_scheduler_recovery() -> anyhow::Result<()> {
    let nats_binary = PathBuf::from(
        std::env::var_os("WAMN_HOST_LIVE_NATS_SERVER_BIN")
            .context("set WAMN_HOST_LIVE_NATS_SERVER_BIN to the real NATS server binary")?,
    );
    ensure!(
        nats_binary.is_absolute() && nats_binary.is_file(),
        "NATS server binary must be an absolute existing file"
    );
    let evidence =
        PathBuf::from(std::env::var_os("WAMN_HOST_LIVE_EVIDENCE_DIR").context(
            "set WAMN_HOST_LIVE_EVIDENCE_DIR to a fresh directory outside the source tree",
        )?);
    ensure!(
        evidence.is_absolute(),
        "evidence directory must be absolute"
    );
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("host crate must be beneath services/")?
        .canonicalize()?;
    let evidence_parent = evidence
        .parent()
        .context("evidence directory needs a parent")?
        .canonicalize()?;
    ensure!(
        !evidence_parent.starts_with(repository),
        "evidence must remain outside the source tree"
    );
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&evidence)
        .context("create the fresh evidence directory")?;
    fs::write(
        evidence.join("host-binary.path"),
        env!("CARGO_BIN_EXE_wamn-host"),
    )?;
    fs::write(
        evidence.join("nats-server-binary.path"),
        nats_binary.as_os_str().as_encoded_bytes(),
    )?;
    let nats_port = reserve_address()?;
    let nats_address = nats_port.local_addr()?;
    drop(nats_port);
    let mut nats = start_nats(&nats_binary, nats_address, &evidence, "nats-initial").await?;
    let result = async {
        assert_missing_first_beat(nats_address, &evidence).await?;
        assert_host_lifecycle(&mut nats, &nats_binary, nats_address, &evidence, "TERM").await?;
        assert_host_lifecycle(&mut nats, &nats_binary, nats_address, &evidence, "INT").await?;
        assert_blocked_flush(nats_address, &evidence).await
    }
    .await;
    if nats.try_wait()?.is_none() {
        timeout(REAP_BUDGET, nats.kill())
            .await
            .context("stop and reap final owned NATS fixture")??;
    }
    result
}
