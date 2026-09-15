//! Own the existing Receiving development fixtures around one timing command.

use std::collections::BTreeMap;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use ring::rand::{SecureRandom as _, SystemRandom};
use rustix::process::{Pid, Signal, kill_process_group};
use serde_json::json;
use tokio::io::AsyncWriteExt as _;
use tokio::process::Command;
use tokio::signal::unix::{SignalKind, signal};
use wamn_control_provision::events::{advisory_stream_config, source_stream_config};
use wamn_control_registry::Triple;
use wamn_test_infrastructure::event_broker;

#[derive(Debug)]
struct Compose {
    repository: PathBuf,
    directory: PathBuf,
    project: String,
}

impl Compose {
    fn command(&self) -> Command {
        let mut command = Command::new("docker");
        command
            .args([
                "compose",
                "--profile",
                "receiving-route",
                "-p",
                &self.project,
                "-f",
            ])
            .arg(
                self.repository
                    .join("test-support/infrastructure/std-virtualization.compose.yaml"),
            )
            .arg("-f")
            .arg(self.directory.join("compose.json"))
            .env("WAMN_STD_VIRT_PG_PORT", "0")
            .env("WAMN_STD_VIRT_REGISTRY_PORT", "0")
            .env("WAMN_ROUTE_REGISTRY_PORT", "0")
            .env("WAMN_RECEIVING_DEV_NATS_PORT", "0")
            .env("WAMN_RECEIVING_DEV_TEMPO_PORT", "0")
            .env("WAMN_RECEIVING_DEV_OTLP_PORT", "0")
            .env(
                "WAMN_ROUTE_REGISTRY_HTPASSWD",
                self.directory.join("htpasswd"),
            )
            .kill_on_drop(true);
        command
    }

    async fn port(&self, service: &str, port: &str) -> anyhow::Result<String> {
        let address = checked(self.command().args(["port", service, port])).await?;
        let address = String::from_utf8(address)?.trim().to_owned();
        let (host, port) = address
            .rsplit_once(':')
            .context("owned service has a published port")?;
        ensure!(
            host == "127.0.0.1" && port.parse::<u16>()? != 0,
            "owned services must bind loopback"
        );
        Ok(address)
    }

    async fn stop(&self) -> anyhow::Result<()> {
        checked(
            self.command()
                .args(["down", "--volumes", "--remove-orphans"]),
        )
        .await
        .context("stop the owned timing services")?;
        ensure!(
            checked(self.command().args(["ps", "--all", "--quiet"]))
                .await?
                .is_empty(),
            "the owned timing project still has containers after shutdown"
        );
        fs::remove_dir_all(&self.directory).context("remove the private timing service files")
    }
}

impl Drop for Compose {
    fn drop(&mut self) {
        if !self.directory.exists() {
            return;
        }
        // The random project was empty before startup. Compose's project label
        // scopes shutdown to this invocation's containers and disposable volumes.
        let _ = self
            .command()
            .args(["down", "--volumes", "--remove-orphans"])
            .as_std_mut()
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?
        .write_all(bytes)?;
    Ok(())
}

async fn checked(command: &mut Command) -> anyhow::Result<Vec<u8>> {
    let output = command.kill_on_drop(true).output().await?;
    ensure!(
        output.status.success(),
        "owned fixture operation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}

/// Run the timing command in its own process group, which a signal to this process stops.
///
/// The command inherits no PostgreSQL variables of this process.
async fn run(command: &mut Command) -> anyhow::Result<std::process::ExitStatus> {
    for (name, _) in std::env::vars_os() {
        if name.to_str().is_some_and(|name| {
            name.starts_with("PG") || name.ends_with("_PG_URL") || name.ends_with("DATABASE_URL")
        }) {
            command.env_remove(name);
        }
    }
    command.kill_on_drop(true).process_group(0);
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    let mut child = command.spawn().context("start the timing command")?;
    let group = Pid::from_raw(i32::try_from(
        child.id().context("the timing command has a process ID")?,
    )?)
    .context("the timing command process ID is positive")?;
    let status = tokio::select! {
        status = child.wait() => status.context("wait for the timing command"),
        _ = interrupt.recv() => Err(anyhow::anyhow!("the timing command was interrupted")),
        _ = terminate.recv() => Err(anyhow::anyhow!("the timing command was terminated")),
        _ = hangup.recv() => Err(anyhow::anyhow!("the timing command lost its session")),
    };
    let _ = kill_process_group(group, Signal::KILL);
    if status.is_err() {
        let _ = child.wait().await;
    }
    status
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let repository = PathBuf::from(
        arguments
            .next()
            .context("usage: delivery_timings BASELINE TARGET RESULT_ROOT -- COMMAND...")?,
    )
    .canonicalize()?;
    let target = PathBuf::from(arguments.next().context("missing baseline target")?);
    let results = PathBuf::from(
        arguments
            .next()
            .context("missing temporary results directory")?,
    );
    ensure!(
        arguments.next().as_deref() == Some(std::ffi::OsStr::new("--")),
        "missing command separator"
    );
    let executable = arguments.next().context("missing timing command")?;
    ensure!(
        target.is_absolute() && results.is_absolute(),
        "target and result paths must be absolute"
    );
    DirBuilder::new()
        .mode(0o700)
        .create(&results)
        .context("reserve a new private timing directory")?;
    let directory = results.join("services");
    DirBuilder::new().mode(0o700).create(&directory)?;
    let mut nonce = [0; 32];
    SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| anyhow::anyhow!("generate timing fixture credentials"))?;
    let compose = Compose {
        repository: repository.clone(),
        directory: directory.clone(),
        project: format!("wamn-delivery-timings-{}", hex::encode(&nonce[..8])),
    };
    let scope = Triple::new("acme", "receiving", "dev");
    let source = source_stream_config(&scope, 1, Duration::from_secs(120));
    let advisory = advisory_stream_config(&scope, 1);
    let broker = event_broker::prepare(
        &directory,
        &scope,
        "receiving-route-auth",
        &source,
        &advisory,
        &[],
    )?;
    private(
        &directory.join("compose.json"),
        &serde_json::to_vec(&json!({"services":{"delivery-events":{
            "image":"nats:2.10-alpine","command":["--config=/etc/nats/nats.conf","--jetstream","--store_dir=/data"],
            "ports":["127.0.0.1::4222"],"volumes":[{"type":"bind","source":broker.configuration,"target":"/etc/nats/nats.conf","read_only":true}],
            "healthcheck":{"test":["CMD-SHELL","wget -qO- http://127.0.0.1:8222/healthz | grep -q ok"],"interval":"1s","timeout":"2s","retries":60}
        }}}))?,
    )?;
    ensure!(
        checked(compose.command().args(["ps", "--all", "--quiet"]))
            .await?
            .is_empty(),
        "timing project already owns containers"
    );
    let password = hex::encode(&nonce[8..]);
    let mut htpasswd = Command::new("docker")
        .args([
            "run",
            "--rm",
            "-i",
            "--name",
            &format!("{}-htpasswd", compose.project),
            "--entrypoint",
            "htpasswd",
            "httpd:2-alpine",
            "-Bni",
            "wamn-timing",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    htpasswd
        .stdin
        .take()
        .context("registry password input")?
        .write_all(format!("{password}\n").as_bytes())
        .await?;
    let output = htpasswd.wait_with_output().await?;
    ensure!(
        output.status.success(),
        "prepare the private registry password"
    );
    private(&directory.join("htpasswd"), &output.stdout)?;
    checked(compose.command().args([
        "up",
        "--detach",
        "--wait",
        "--wait-timeout",
        "90",
        "authenticated-registry",
        "receiving-dev-nats",
        "receiving-dev-tempo",
        "delivery-events",
    ]))
    .await?;
    let registry = compose.port("authenticated-registry", "5000").await?;
    let scheduler = compose.port("receiving-dev-nats", "4222").await?;
    let events = compose.port("delivery-events", "4222").await?;
    let tempo = compose.port("receiving-dev-tempo", "3200").await?;
    let otlp = compose.port("receiving-dev-tempo", "4317").await?;
    let registry_auth = directory.join("registry-auth.json");
    private(
        &registry_auth,
        &serde_json::to_vec(
            &json!({"auths":{registry.clone():{"username":"wamn-timing","password":password}}}),
        )?,
    )?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    ensure!(
        client
            .get(format!("http://{registry}/v2/"))
            .send()
            .await?
            .status()
            == reqwest::StatusCode::UNAUTHORIZED,
        "owned registry must refuse an anonymous request"
    );
    ensure!(
        client
            .get(format!("http://{registry}/v2/"))
            .basic_auth("wamn-timing", Some(&password))
            .send()
            .await?
            .status()
            .is_success(),
        "owned registry must accept its private credential"
    );
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if client
                .get(format!("http://{tempo}/ready"))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await
    .context("owned Tempo readiness")?;
    let event_url = format!("nats://{events}");
    event_broker::connect(&broker.provisioning, &event_url).await?;
    event_broker::connect(&broker.runtime, &event_url).await?;
    let variables = BTreeMap::from([
        ("WAMN_TIMINGS_ROOT", results.display().to_string()),
        ("WAMN_TIMINGS_BASELINE", repository.display().to_string()),
        (
            "WAMN_RECEIVING_DEV_BIN",
            target.join("debug/wamn").display().to_string(),
        ),
        (
            "WAMN_RECEIVING_DEV_HOST_BIN",
            target.join("debug/wamn-host").display().to_string(),
        ),
        (
            "WAMN_JOURNEY_SCENARIO_WORKER_BIN",
            target
                .join("debug/wamn-scenario-worker")
                .display()
                .to_string(),
        ),
        (
            "WAMN_IDENTITY_BINARY",
            target.join("debug/wamn-identity").display().to_string(),
        ),
        ("WAMN_RECEIVING_DEV_NATS_URL", format!("nats://{scheduler}")),
        ("WAMN_EVT_NATS_URL", event_url),
        ("WAMN_EVT_NATS_USERNAME", broker.runtime.username),
        (
            "WAMN_EVT_NATS_PASSWORD_FILE",
            broker.runtime.password_file.display().to_string(),
        ),
        (
            "WAMN_DEV_ENV_EVENT_PROVISIONING_USERNAME",
            broker.provisioning.username,
        ),
        (
            "WAMN_DEV_ENV_EVENT_PROVISIONING_PASSWORD_FILE",
            broker.provisioning.password_file.display().to_string(),
        ),
        ("WAMN_EVT_STREAM_REPLICAS", "1".to_owned()),
        ("WAMN_EVT_DUP_WINDOW_SECS", "120".to_owned()),
        (
            "WAMN_RECEIVING_DEV_TEMPO_QUERY_URL",
            format!("http://{tempo}"),
        ),
        (
            "WAMN_RECEIVING_DEV_OTEL_EXPORTER_OTLP_ENDPOINT",
            format!("http://{otlp}"),
        ),
        (
            "WAMN_RECEIVING_DEV_FLOW_HTTP_WORKLOAD_IMAGE",
            format!("{registry}/wamn/flow-http:timing"),
        ),
        (
            "WAMN_ROUTE_COMPONENT_ARTIFACT_BASE",
            format!("{registry}/wamn/components"),
        ),
        (
            "WAMN_ROUTE_RELEASE_ARTIFACT_BASE",
            format!("{registry}/wamn/releases"),
        ),
        ("WAMN_ROUTE_HOST", "receiving.localhost".to_owned()),
        (
            "WAMN_ROUTE_REGISTRY_AUTH_FILE",
            registry_auth.display().to_string(),
        ),
    ]);
    let mut command = Command::new(executable);
    command
        .args(arguments)
        .current_dir(&repository)
        .envs(&variables)
        .env("CARGO_TARGET_DIR", &target)
        .env("CARGO_BUILD_JOBS", "2");
    let status = run(&mut command).await?;
    compose.stop().await?;
    ensure!(status.success(), "timing command failed with {status}");
    Ok(())
}
