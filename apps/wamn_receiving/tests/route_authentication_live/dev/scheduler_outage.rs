//! Routes answer during a scheduler NATS outage and after it ends
//! (wamn-7k2fh; the outage stage that b8199fb4c deleted).
//!
//! The developer command (`wamn dev`, which activates through
//! `dev::activation::activate`) starts the host with the local Receiving
//! application. Its scheduler NATS is a container that this test starts,
//! stops and starts again, as `services/host/tests/scheduler_nats_move_live.rs`
//! does. During the outage the route must answer every request; after it the
//! host must publish its heartbeat through the restarted server, with no host
//! restart, and the route must still answer.
//!
//! Out of target: the Kubernetes half of the deleted case, where every Host and
//! WorkloadDeployment is ready again after the outage. That state belongs to
//! the operator and the cluster, so no test without a cluster can observe it.

use std::net::{Ipv4Addr, TcpListener};
use std::time::Duration;

use anyhow::{Context as _, ensure};
use futures_util::StreamExt as _;
use tokio::process::Command;
use tokio::time::{Instant, sleep, timeout};

use super::super::{PLATFORM_DOMAIN, ScratchRoot, connect, write_dev_config};
use super::DevJourneyInputs;
use super::local_delivery::Watch;

/// The image of the developer scheduler NATS
/// (test-support/infrastructure/std-virtualization.compose.yaml).
const NATS_IMAGE: &str = "nats:2.10-alpine";
/// Longer than several passes at the 4-second reconnect delay of async-nats.
const OUTAGE: Duration = Duration::from_secs(20);
/// The recovery bound of the deleted case.
const RECOVERY_BUDGET: Duration = Duration::from_secs(120);

/// The one NATS container, with TLS and a full-permission development
/// certificate. Dropping it removes the container.
struct Scheduler {
    name: String,
    port: u16,
    certificate: wamn_test_infrastructure::nats_tls::ClientCertificate,
    _directory: ScratchRoot,
}

impl Scheduler {
    async fn start() -> anyhow::Result<Self> {
        // A fixed host port, so the restarted container keeps the address the
        // host connects to.
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?
            .local_addr()?
            .port();
        // The configuration names its certificates by absolute path, so the
        // container sees the directory at the same path.
        let directory = ScratchRoot::create()?;
        let authority = wamn_test_infrastructure::nats_tls::Authority::new(directory.path())?;
        let certificate = authority.client(wamn_test_infrastructure::nats_tls::DEVELOPMENT_USER)?;
        let configuration = directory.path().join("nats.conf");
        std::fs::write(
            &configuration,
            serde_json::to_vec(&serde_json::json!({
                "tls": authority.server_tls(),
                "authorization": {"users": [wamn_test_infrastructure::nats_tls::full_permission_user(
                    wamn_test_infrastructure::nats_tls::DEVELOPMENT_USER
                )]}
            }))?,
        )?;
        let mount = format!("{0}:{0}:ro", directory.path().display());
        let scheduler = Self {
            name: format!("wamn-scheduler-outage-{}", std::process::id()),
            port,
            certificate,
            _directory: directory,
        };
        docker(&[
            "run",
            "-d",
            "--name",
            &scheduler.name,
            "-p",
            &format!("127.0.0.1:{port}:4222"),
            "--volume",
            &mount,
            NATS_IMAGE,
            "--config",
            &configuration.display().to_string(),
            "--jetstream",
            "--store_dir=/data",
        ])
        .await?;
        scheduler.ready().await?;
        Ok(scheduler)
    }

    fn url(&self) -> String {
        format!("nats://127.0.0.1:{}", self.port)
    }

    async fn ready(&self) -> anyhow::Result<async_nats::Client> {
        timeout(Duration::from_secs(60), async {
            loop {
                if let Ok(client) = self.certificate.connect(&self.url()).await {
                    return client;
                }
                sleep(Duration::from_millis(250)).await;
            }
        })
        .await
        .context("the scheduler NATS container did not accept connections")
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        let _ = std::process::Command::new("docker")
            .args(["rm", "-f", &self.name])
            .output();
    }
}

async fn docker(args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("docker").args(args).output().await?;
    ensure!(
        output.status.success(),
        "docker {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

#[tokio::test]
#[ignore = "requires: env:WAMN_RECEIVING_DEV_BIN, env:WAMN_DEV_ENV_FLOW_HTTP_COMPONENT, env:WAMN_DEV_ENV_MATERIALIZER_COMPONENT, env:WAMN_RECEIVING_DEV_HOST_BIN, env:WAMN_RECEIVING_DEV_NATS_URL, env:WAMN_DEV_ENV_NATS_TLS_CA, env:WAMN_DEV_ENV_NATS_TLS_CERT, env:WAMN_DEV_ENV_NATS_TLS_KEY, env:WAMN_EVT_STREAM_REPLICAS, env:WAMN_EVT_DUP_WINDOW_SECS, env:WAMN_RECEIVING_DEV_OTEL_EXPORTER_OTLP_ENDPOINT, env:WAMN_ROUTE_HOST, bin:cargo-sqlx, bin:jq, bin:docker"]
async fn routes_answer_during_a_scheduler_outage_and_recover() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: env:WAMN_RECEIVING_DEV_BIN, env:WAMN_DEV_ENV_FLOW_HTTP_COMPONENT, env:WAMN_DEV_ENV_MATERIALIZER_COMPONENT, env:WAMN_RECEIVING_DEV_HOST_BIN, env:WAMN_RECEIVING_DEV_NATS_URL, env:WAMN_DEV_ENV_NATS_TLS_CA, env:WAMN_DEV_ENV_NATS_TLS_CERT, env:WAMN_DEV_ENV_NATS_TLS_KEY, env:WAMN_EVT_STREAM_REPLICAS, env:WAMN_EVT_DUP_WINDOW_SECS, env:WAMN_RECEIVING_DEV_OTEL_EXPORTER_OTLP_ENDPOINT, env:WAMN_ROUTE_HOST, bin:cargo-sqlx, bin:jq, bin:docker",
    );
    let repository = super::super::repository_root()?;
    let scheduler = Scheduler::start().await?;
    // The environment resets the control store of the whole server, so the test starts its own.
    let mut server = wamn_test_infrastructure::postgres::start(&[])?;
    let system_url = server.create_database("wamn_system")?.url().to_owned();
    let mut inputs = DevJourneyInputs::required()?;
    // One NATS: the outage stops the scheduler, the events and the memories.
    inputs.environment.nats = scheduler.certificate.platform_nats(&scheduler.url());
    let scratch = ScratchRoot::create()?;
    let root = scratch.path();
    inputs.environment.local_artifacts.directory = root.join("local-artifacts");
    let mut admin = connect(&system_url).await?;
    std::fs::write(
        root.join("environment.k"),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../tests/integration/fixtures/local-environments/acme-receiving.k"
        )),
    )?;
    let environment = wamn_dev_control::dev::environment::provision(
        &system_url,
        &mut admin,
        root,
        PLATFORM_DOMAIN,
        &wamn_environment_control::environment::document::EnvironmentDocument::compile(
            &root.join("environment.k"),
        )?,
    )
    .await?;
    let provisioning = inputs.environment.nats.connect().await?;
    wamn_broker::streams::provision(
        &async_nats::jetstream::new(provisioning.clone()),
        &wamn_registry::Triple::new(
            &environment.identity.org,
            environment.identity.environment.as_str(),
            &environment.identity.project,
        ),
        inputs.environment.stream_replicas,
        Duration::from_secs(inputs.environment.dup_window_secs),
        &[],
    )
    .await?;

    let config = write_dev_config(
        root,
        &system_url,
        &environment.route,
        &environment.credentials,
        &inputs.environment,
        &environment.identity,
    )?;
    let mut watch = Watch::start(&inputs.wamn_binary, &repository, &config)?;
    let result = async {
        let endpoint = watch.served().await?;
        let mut project = connect(&environment.route.database_url).await?;
        super::super::environment::seed_receiving_business_rows(&mut project).await?;
        let outage = async {
        let route = Route {
            http: reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(30))
                .build()?,
            url: format!("{}/location/list", endpoint.url),
            host: endpoint.host.clone(),
            token: environment.route.token.clone(),
        };
        route.answers("before the outage").await?;

        docker(&["stop", &scheduler.name]).await?;
        let stopped = Instant::now();
        let mut answered = 0;
        let mut slowest = Duration::ZERO;
        while stopped.elapsed() < OUTAGE {
            let sent = Instant::now();
            route.answers("during the scheduler outage").await?;
            slowest = slowest.max(sent.elapsed());
            answered += 1;
            sleep(Duration::from_millis(500)).await;
        }

        docker(&["start", &scheduler.name]).await?;
        let restarted = Instant::now();
        let operator = scheduler.ready().await?;
        let mut heartbeats = operator.subscribe("runtime.operator.heartbeat.*").await?;
        let heartbeat = timeout(RECOVERY_BUDGET, heartbeats.next())
            .await
            .context("the host published no heartbeat through the restarted scheduler NATS")?
            .context("the heartbeat subscription ended")?;
        let recovered_after = restarted.elapsed();
        let heartbeat = String::from_utf8(heartbeat.payload.to_vec())?;
        ensure!(
            heartbeat.contains(environment.identity.host_name.as_str()),
            "the heartbeat after the outage is not this host's: {heartbeat}"
        );
        route.answers("after the scheduler returned").await?;
        println!(
            "SCHEDULER_OUTAGE result=pass outage_seconds={} answered_during_outage={answered} slowest_answer_ms={} heartbeat_after_restart_ms={}",
            OUTAGE.as_secs(),
            slowest.as_millis(),
            recovered_after.as_millis()
        );
        Ok::<_, anyhow::Error>(())
        };
        // The host logs through the developer command's pipes, so the test
        // reads them while the outage runs; a full pipe would stall the host.
        tokio::pin!(outage);
        loop {
            tokio::select! {
                result = &mut outage => break result,
                line = watch.line() => {
                    line?;
                }
            }
        }
    }
    .await;
    let stopped = watch.stop().await;
    let result = result.with_context(|| {
        format!(
            "local developer stderr tail:\n{}",
            watch
                .diagnostics
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        )
    });
    drop(watch);

    provisioning.drain().await?;
    drop(scheduler);
    result?;
    stopped
}

/// The authenticated location read of the served route.
struct Route {
    http: reqwest::Client,
    url: String,
    host: String,
    token: String,
}

impl Route {
    async fn answers(&self, when: &str) -> anyhow::Result<()> {
        let response = self
            .http
            .get(&self.url)
            .header("Host", &self.host)
            .bearer_auth(&self.token)
            .send()
            .await
            .with_context(|| format!("the route did not answer {when}"))?;
        ensure!(
            response.status() == reqwest::StatusCode::OK,
            "the route answered {} {when}",
            response.status()
        );
        let body: serde_json::Value = response.json().await?;
        ensure!(
            body[0]["value"]["rows"][0]["location_code"] == "DOCK-1",
            "the route answered the wrong rows {when}: {body}"
        );
        Ok(())
    }
}
