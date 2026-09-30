//! Scheduler NATS outage tests for the rebuilt host (docs/plan/nats-outage.md,
//! issue 1).
//!
//! The ignored tests run NATS in a Docker container on its own network, under
//! the alias `nats` and with TLS as the runtime-operator chart configures it.
//! The host runs in a second container on that network against
//! `nats://nats:4222`. The move case stops and removes the NATS container,
//! parks a placeholder on its address, and starts a new NATS container under
//! the same alias, so the name resolves to a new IP. The pause case first
//! pauses the server, which keeps its connections open and answers nothing,
//! measures what the client does, unpauses it, and then moves it. Each case
//! requires a heartbeat RPC through the new server with no host restart, and a
//! debug line with the cause of each failed attempt. They require Docker, the
//! images below and an evidence directory outside the source tree, in which
//! each case creates its own new directory. They start no PostgreSQL, operator
//! or guest workload.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::process::Command;
use tokio::time::{Instant, sleep, timeout};

/// The chart's NATS image (charts/runtime-operator/values.yaml, `nats.image`).
const NATS_IMAGE: &str = "nats:2.12.8-alpine";
/// A base whose glibc runs the host binary that Cargo built for this test.
const HOST_IMAGE: &str = "debian:trixie-slim";
const REQUEST_BUDGET: Duration = Duration::from_secs(1);
const STARTUP_BUDGET: Duration = Duration::from_secs(60);
/// Longer than several passes at the 4-second reconnect delay of async-nats.
const NATS_OUTAGE: Duration = Duration::from_secs(20);
const RECOVERY_BUDGET: Duration = Duration::from_secs(60);

/// The chart's server configuration (templates/nats/config.yaml) with TLS on.
const NATS_CONFIG: &str = r#"port: 4222
jetstream {
  store_dir: "/data/jetstream"
}
tls {
  cert_file: /nats-cert/tls.crt
  key_file: /nats-cert/tls.key
  ca_file: /nats-cert/ca.crt
  verify_and_map: true
}
authorization {
  users = [
    { user: "wasmcloud-operator" },
    { user: "wasmcloud-runtime" },
    { user: "wasmcloud-data" }
  ]
}
"#;

/// Removes every container and the network that this run named, also when an
/// assertion fails.
struct Fixture {
    prefix: String,
    containers: Vec<String>,
}

impl Fixture {
    fn network(&self) -> String {
        format!("{}-net", self.prefix)
    }

    fn container(&mut self, role: &str) -> String {
        let name = format!("{}-{role}", self.prefix);
        self.containers.push(name.clone());
        name
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for name in &self.containers {
            let _ = StdCommand::new("docker").args(["rm", "-f", name]).output();
        }
        let _ = StdCommand::new("docker")
            .args(["network", "rm", &self.network()])
            .output();
    }
}

async fn docker(args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("docker")
        .args(args)
        .kill_on_drop(true)
        .output()
        .await
        .context("run docker")?;
    ensure!(
        output.status.success(),
        "docker {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

async fn container_ip(name: &str, network: &str) -> anyhow::Result<String> {
    let format = format!("{{{{(index .NetworkSettings.Networks \"{network}\").IPAddress}}}}");
    docker(&["inspect", "-f", &format, name]).await
}

/// The loopback port that Docker published for a container port.
async fn published_port(name: &str, port: &str) -> anyhow::Result<u16> {
    let mapping = docker(&["port", name, port]).await?;
    let first = mapping.lines().next().context("no published port")?;
    let (_, port) = first.rsplit_once(':').context("malformed port mapping")?;
    Ok(port.parse()?)
}

struct Certificates {
    ca: PathBuf,
    operator_cert: PathBuf,
    operator_key: PathBuf,
}

/// The chart's certificate set (templates/certificates.yaml): one CA, a server
/// certificate for `nats`, and client certificates whose names map to users.
/// The server also names `localhost`, because this test reaches NATS through a
/// published loopback port.
fn certificates(nats_dir: &Path, runtime_dir: &Path) -> anyhow::Result<Certificates> {
    let mut ca_params = CertificateParams::new(Vec::<String>::new())?;
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "wasmcloud CA");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    let ca_key = KeyPair::generate()?;
    let ca = ca_params.self_signed(&ca_key)?;
    let issuer = Issuer::new(ca_params, ca_key);
    let signed = |common_name: &str, names: Vec<String>| -> anyhow::Result<(String, String)> {
        let key = KeyPair::generate()?;
        let mut params = CertificateParams::new(names)?;
        params
            .distinguished_name
            .push(DnType::CommonName, common_name);
        let cert = params.signed_by(&key, &issuer)?;
        Ok((cert.pem(), key.serialize_pem()))
    };
    let (nats_cert, nats_key) = signed("nats", vec!["nats".to_owned(), "localhost".to_owned()])?;
    let (runtime_cert, runtime_key) =
        signed("wasmcloud-runtime", vec!["wasmcloud-runtime".to_owned()])?;
    let (operator_cert, operator_key) =
        signed("wasmcloud-operator", vec!["wasmcloud-operator".to_owned()])?;
    for (dir, cert, key) in [
        (nats_dir, &nats_cert, &nats_key),
        (runtime_dir, &runtime_cert, &runtime_key),
    ] {
        fs::write(dir.join("ca.crt"), ca.pem())?;
        fs::write(dir.join("tls.crt"), cert)?;
        fs::write(dir.join("tls.key"), key)?;
    }
    let operator_dir = runtime_dir
        .parent()
        .context("certificate directory needs a parent")?
        .join("operator-cert");
    fs::create_dir(&operator_dir)?;
    fs::write(operator_dir.join("ca.crt"), ca.pem())?;
    fs::write(operator_dir.join("tls.crt"), operator_cert)?;
    fs::write(operator_dir.join("tls.key"), operator_key)?;
    Ok(Certificates {
        ca: operator_dir.join("ca.crt"),
        operator_cert: operator_dir.join("tls.crt"),
        operator_key: operator_dir.join("tls.key"),
    })
}

/// A client of the test process, as the operator connects: its own
/// certificate, through the published port of the current NATS container.
async fn operator_client(certs: &Certificates, port: u16) -> anyhow::Result<async_nats::Client> {
    Ok(async_nats::ConnectOptions::new()
        .add_root_certificates(certs.ca.clone())
        .add_client_certificate(certs.operator_cert.clone(), certs.operator_key.clone())
        .connect(format!("tls://localhost:{port}"))
        .await?)
}

async fn start_nats(
    fixture: &mut Fixture,
    role: &str,
    evidence: &Path,
    certs: &Certificates,
) -> anyhow::Result<(String, u16)> {
    let name = fixture.container(role);
    let network = fixture.network();
    let cert_mount = format!("{}:/nats-cert:ro", evidence.join("nats-cert").display());
    let config_mount = format!(
        "{}:/etc/nats/nats-server.conf:ro",
        evidence.join("nats-server.conf").display()
    );
    docker(&[
        "run",
        "-d",
        "--name",
        &name,
        "--network",
        &network,
        "--network-alias",
        "nats",
        "-p",
        "127.0.0.1::4222",
        "-v",
        &cert_mount,
        "-v",
        &config_mount,
        NATS_IMAGE,
    ])
    .await?;
    let port = published_port(&name, "4222/tcp").await?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(Ok(client)) = timeout(REQUEST_BUDGET, operator_client(certs, port)).await {
            timeout(REQUEST_BUDGET, client.flush()).await??;
            return Ok((name, port));
        }
        ensure!(
            Instant::now() < deadline,
            "NATS container {name} did not become ready"
        );
        sleep(Duration::from_millis(100)).await;
    }
}

async fn probe(port: u16, path: &str) -> anyhow::Result<(u16, String)> {
    timeout(REQUEST_BUDGET, async {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
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
    .context("host probe exceeded one second")?
}

async fn save_host_log(host: &str, evidence: &Path) -> anyhow::Result<String> {
    let output = Command::new("docker")
        .args(["logs", host])
        .kill_on_drop(true)
        .output()
        .await?;
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    fs::write(evidence.join("host.log"), &log)?;
    Ok(log)
}

fn native_host_id(log: &str) -> Option<String> {
    log.lines()
        .filter(|line| line.contains("Host started"))
        .find_map(|line| {
            let (_, value) = line.split_once("host_id=")?;
            let id = value
                .trim_start_matches('"')
                .split(|c: char| c == '"' || c.is_whitespace())
                .next()?;
            (!id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
                .then(|| id.to_owned())
        })
}

async fn heartbeat_rpc(
    certs: &Certificates,
    port: u16,
    host_id: &str,
    host_name: &str,
    budget: Duration,
) -> anyhow::Result<Vec<u8>> {
    timeout(budget, async {
        let client = operator_client(certs, port).await?;
        let subject = wash_runtime::washlet::rpc_subject(host_id, "heartbeat");
        loop {
            if let Ok(Ok(response)) = timeout(
                REQUEST_BUDGET,
                client.request(subject.clone(), Vec::new().into()),
            )
            .await
            {
                let body = std::str::from_utf8(&response.payload)?;
                ensure!(
                    body.contains(host_name),
                    "heartbeat RPC did not name this host"
                );
                return Ok(response.payload.to_vec());
            }
            sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .context("heartbeat RPC did not answer before its deadline")?
}

/// One case: its evidence directory, its certificates, its containers, and a
/// ready host connected to the first NATS container.
struct Case {
    evidence: PathBuf,
    certs: Certificates,
    fixture: Fixture,
    network: String,
    host: String,
    probe_port: u16,
    host_id: String,
    nats: String,
    nats_ip: String,
    nats_port: u16,
}

const HOST_NAME: &str = "nats-move-host";

/// Creates `<WAMN_HOST_LIVE_EVIDENCE_DIR>/<case>`, which must be new, starts
/// the first NATS container and the host, and waits for a heartbeat RPC.
async fn start_case(case: &str) -> anyhow::Result<Case> {
    wamn_test_postgres::require_prerequisites(&["WAMN_HOST_LIVE_EVIDENCE_DIR"]);
    // The host installs its provider through wash-runtime; this process has two.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let parent = PathBuf::from(
        std::env::var_os("WAMN_HOST_LIVE_EVIDENCE_DIR")
            .context("set WAMN_HOST_LIVE_EVIDENCE_DIR to a directory outside the source tree")?,
    );
    ensure!(parent.is_absolute(), "evidence directory must be absolute");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&parent)?;
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("host crate must be beneath services/")?
        .canonicalize()?;
    ensure!(
        !parent.canonicalize()?.starts_with(repository),
        "evidence must remain outside the source tree"
    );
    let evidence = parent.join(case);
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&evidence)
        .context("create the fresh case evidence directory")?;
    fs::create_dir(evidence.join("nats-cert"))?;
    fs::create_dir(evidence.join("runtime-cert"))?;
    fs::write(evidence.join("nats-server.conf"), NATS_CONFIG)?;
    let certs = certificates(&evidence.join("nats-cert"), &evidence.join("runtime-cert"))?;

    let mut fixture = Fixture {
        prefix: format!("wamn-nats-move-{}-{case}", std::process::id()),
        containers: Vec::new(),
    };
    let network = fixture.network();
    docker(&["network", "create", &network]).await?;
    let (nats, nats_port) = start_nats(&mut fixture, "nats-first", &evidence, &certs).await?;
    let nats_ip = container_ip(&nats, &network).await?;

    let host = fixture.container("host");
    let binary_mount = format!("{}:/wamn-host:ro", env!("CARGO_BIN_EXE_wamn-host"));
    let cert_mount = format!(
        "{}:/runtime-cert:ro",
        evidence.join("runtime-cert").display()
    );
    docker(&[
        "run",
        "-d",
        "--name",
        &host,
        "--network",
        &network,
        "-p",
        "127.0.0.1::9090",
        "-v",
        &binary_mount,
        "-v",
        &cert_mount,
        // The HTTP transport loads the system roots at startup.
        "-v",
        "/etc/ssl/certs/ca-certificates.crt:/etc/ssl/certs/ca-certificates.crt:ro",
        HOST_IMAGE,
        "/wamn-host",
        "host",
        "--host-name",
        HOST_NAME,
        "--host-group",
        "nats-move-test",
        "--environment",
        "nats-move-test",
        "--scheduler-nats-url",
        "nats://nats:4222",
        "--nats-connect-timeout",
        "60s",
        "--scheduler-nats-tls-ca",
        "/runtime-cert/ca.crt",
        "--scheduler-nats-tls-cert",
        "/runtime-cert/tls.crt",
        "--scheduler-nats-tls-key",
        "/runtime-cert/tls.key",
        "--http-addr",
        "0.0.0.0:8080",
        "--probe-addr",
        "0.0.0.0:9090",
        "--max-http-ingress-connections",
        "1",
        "--max-concurrent-starts",
        "1",
        "--guest-memory-mode",
        "count",
        "--drain-delay",
        "5s",
    ])
    .await?;
    let probe_port = published_port(&host, "9090/tcp").await?;

    let deadline = Instant::now() + STARTUP_BUDGET;
    let host_id = loop {
        let log = save_host_log(&host, &evidence).await?;
        if probe(probe_port, "/readyz").await.ok() == Some((200, "ok\n".to_owned()))
            && let Some(id) = native_host_id(&log)
        {
            break id;
        }
        ensure!(
            Instant::now() < deadline,
            "host did not become ready; see host.log"
        );
        sleep(Duration::from_millis(250)).await;
    };
    fs::write(
        evidence.join("heartbeat-before.json"),
        heartbeat_rpc(&certs, nats_port, &host_id, HOST_NAME, STARTUP_BUDGET).await?,
    )?;
    Ok(Case {
        evidence,
        certs,
        fixture,
        network,
        host,
        probe_port,
        host_id,
        nats,
        nats_ip,
        nats_port,
    })
}

async fn expect_live(case: &Case) -> anyhow::Result<()> {
    ensure!(
        probe(case.probe_port, "/livez").await? == (200, "ok\n".to_owned()),
        "host liveness failed while scheduler NATS was unavailable"
    );
    Ok(())
}

/// The move: the old server goes, a placeholder takes its address, and a new
/// server answers under the same alias on another address. Returns the result
/// lines of the move.
async fn move_nats(case: &mut Case) -> anyhow::Result<String> {
    docker(&["stop", &case.nats]).await?;
    docker(&["rm", &case.nats]).await?;
    let stopped = Instant::now();
    let placeholder = case.fixture.container("placeholder");
    docker(&[
        "run",
        "-d",
        "--name",
        &placeholder,
        "--network",
        &case.network,
        HOST_IMAGE,
        "sleep",
        "infinity",
    ])
    .await?;
    while stopped.elapsed() < NATS_OUTAGE {
        expect_live(case).await?;
        sleep(Duration::from_millis(500)).await;
    }
    let (second_nats, second_port) = start_nats(
        &mut case.fixture,
        "nats-second",
        &case.evidence,
        &case.certs,
    )
    .await?;
    let second_ip = container_ip(&second_nats, &case.network).await?;
    ensure!(
        case.nats_ip != second_ip,
        "the new NATS container kept the address {}",
        case.nats_ip
    );
    let restarted = Instant::now();
    let recovery = heartbeat_rpc(
        &case.certs,
        second_port,
        &case.host_id,
        HOST_NAME,
        RECOVERY_BUDGET,
    )
    .await;
    let recovered_after = restarted.elapsed();
    let lines = format!(
        "move_first_ip={}\nmove_second_ip={second_ip}\nmove_outage_ms={}\nmove_recovered={}\nmove_recovered_after_ms={}\n",
        case.nats_ip,
        restarted.duration_since(stopped).as_millis(),
        recovery.is_ok(),
        recovered_after.as_millis(),
    );
    fs::write(case.evidence.join("result-move.txt"), &lines)?;
    fs::write(case.evidence.join("heartbeat-after.json"), recovery?)?;
    Ok(lines)
}

/// The failed attempts of the log, counted by cause.
fn causes(log: &str) -> BTreeMap<String, usize> {
    let mut causes = BTreeMap::new();
    for line in log
        .lines()
        .filter(|line| line.contains("connection attempt failed"))
    {
        let cause = line
            .split_once("error=")
            .map_or("<no cause>", |(_, cause)| cause);
        *causes.entry(cause.to_owned()).or_insert(0) += 1;
    }
    causes
}

/// Every failed attempt names its cause, every scheduler client line names its
/// client, and the host never restarted.
async fn finish(case: &Case, result: String) -> anyhow::Result<()> {
    let log = save_host_log(&case.host, &case.evidence).await?;
    let causes = causes(&log);
    let mut result = result;
    for (cause, count) in &causes {
        writeln!(result, "cause {count} x {cause}")?;
    }
    fs::write(case.evidence.join("result.txt"), result)?;
    ensure!(
        !causes.is_empty() && !causes.contains_key("<no cause>"),
        "the host log did not name the cause of each failed attempt; see host.log"
    );
    ensure!(
        log.lines()
            .filter(|line| line.contains("scheduler NATS client error"))
            .all(|line| line.contains(&format!("client={HOST_NAME}"))),
        "a scheduler client error line did not name its client"
    );
    ensure!(
        docker(&[
            "inspect",
            "-f",
            "{{.RestartCount}} {{.State.Running}}",
            &case.host
        ])
        .await?
            == "0 true",
        "the host container restarted or stopped"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires: Docker, nats:2.12.8-alpine, debian:trixie-slim, WAMN_HOST_LIVE_EVIDENCE_DIR"]
async fn host_connects_again_after_scheduler_nats_moves() -> anyhow::Result<()> {
    let mut case = start_case("move").await?;
    let result = move_nats(&mut case).await?;
    finish(&case, result).await
}

/// How long the case waits for the client to notice a server that holds its
/// connection open and answers nothing. The async-nats defaults are a ping
/// every 60 s and two outstanding pings.
const PAUSE_DETECT_BUDGET: Duration = Duration::from_secs(240);

/// A paused server keeps its TCP connections open and answers nothing, as a
/// node that vanishes does. The case measures when the client notices, what
/// its attempts meet, and whether it connects again on unpause, then moves.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires: Docker, nats:2.12.8-alpine, debian:trixie-slim, WAMN_HOST_LIVE_EVIDENCE_DIR"]
async fn host_connects_again_after_scheduler_nats_pauses() -> anyhow::Result<()> {
    let mut case = start_case("pause").await?;
    let disconnects = |log: &str| {
        log.lines()
            .filter(|line| line.contains("disconnected from scheduler NATS"))
            .count()
    };
    let before = disconnects(&save_host_log(&case.host, &case.evidence).await?);
    docker(&["pause", &case.nats]).await?;
    let paused = Instant::now();
    let detected = loop {
        expect_live(&case).await?;
        if disconnects(&save_host_log(&case.host, &case.evidence).await?) > before {
            break Some(paused.elapsed());
        }
        if paused.elapsed() > PAUSE_DETECT_BUDGET {
            break None;
        }
        sleep(Duration::from_secs(1)).await;
    };
    let noticed = Instant::now();
    while detected.is_some() && noticed.elapsed() < NATS_OUTAGE {
        expect_live(&case).await?;
        sleep(Duration::from_millis(500)).await;
    }
    let paused_log = save_host_log(&case.host, &case.evidence).await?;
    fs::write(case.evidence.join("host-paused.log"), &paused_log)?;
    let paused_attempts: usize = causes(&paused_log).values().sum();

    docker(&["unpause", &case.nats]).await?;
    let unpaused = Instant::now();
    let rejoined = heartbeat_rpc(
        &case.certs,
        case.nats_port,
        &case.host_id,
        HOST_NAME,
        RECOVERY_BUDGET,
    )
    .await;
    let rejoined_after = unpaused.elapsed();
    let mut result = format!(
        "pause_disconnect_noticed_after_ms={}\npause_failed_attempts={paused_attempts}\nunpause_heartbeat={}\nunpause_heartbeat_after_ms={}\n",
        detected.map_or("none".to_owned(), |after| after.as_millis().to_string()),
        rejoined.is_ok(),
        rejoined_after.as_millis(),
    );
    fs::write(case.evidence.join("result-pause.txt"), &result)?;
    result.push_str(&move_nats(&mut case).await?);
    finish(&case, result).await
}
