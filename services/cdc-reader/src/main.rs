//! Run change capture from one mounted JSON configuration.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use clap::Parser;
use tokio_util::sync::CancellationToken;
use wamn_broker::nats::{CA_FILE, PlatformNats};
use wamn_broker::publisher::JetStreamPublisher;
use wamn_cdc_reader::replication::ReplicationSource;
use wamn_event::reader::ReaderConfig;
use wamn_registry::Triple;

#[derive(Parser)]
#[command(name = "wamn-cdc-reader", version, about)]
struct Cli {
    /// Mounted reader settings as JSON.
    #[arg(long)]
    settings: PathBuf,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    org: wamn_registry::OrgId,
    project: wamn_registry::ProjectId,
    env: wamn_registry::EnvName,
    database: wamn_postgres::database::ConnectionConfig,
    cdc_name: String,
    nats_url: String,
    nats_creds: PathBuf,
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .log_internal_errors(false)
        .init();
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_tracing();
    // As wash_runtime::init_crypto does for the host. An earlier install
    // leaves its provider in place.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let bytes = std::fs::read(&cli.settings)
        .with_context(|| format!("read reader settings {}", cli.settings.display()))?;
    let settings: Settings = serde_json::from_slice(&bytes).context("decode reader settings")?;
    let stream = wamn_event::stream_name(&settings.org, &settings.project, &settings.env);
    let database = settings.database.to_json()?;
    let token = CancellationToken::new();
    let signalled = token.clone();
    // The process can run as PID 1, which needs a signal handler.
    tokio::spawn(async move {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
        signalled.cancel();
    });

    let source = tokio::select! {
        biased;
        () = token.cancelled() => return Ok(()),
        result = ReplicationSource::prepare(&database, settings.cdc_name) => Arc::new(result?),
    };
    let broker = PlatformNats {
        nats_url: settings.nats_url,
        nats_tls_ca: CA_FILE.into(),
        creds: settings.nats_creds,
    };
    let publisher = JetStreamPublisher::new(broker.connect().await?, stream);
    let config = ReaderConfig {
        target: Triple::new(settings.org, settings.env, settings.project),
        stall_threshold: Duration::from_secs(30),
    };
    wamn_cdc_reader::run(&source, &config, &publisher, &token).await
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::fs::OpenOptions;
    use std::process::Command;

    #[test]
    fn logging_survives_output_failure() {
        const CHILD: &str = "WAMN_CDC_READER_LOG_FAILURE_CHILD";
        if std::env::var_os(CHILD).is_some() {
            super::init_tracing();
            tracing::info!("the failed log write must not panic");
            return;
        }

        // /dev/full fails both the event write and any internal diagnostic.
        // A subprocess keeps the failing stderr away from the parent harness.
        let stderr = OpenOptions::new().write(true).open("/dev/full").unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::logging_survives_output_failure",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("RUST_LOG", "info")
            .stderr(stderr)
            .output()
            .expect("run the logging test with failing stderr");
        assert!(
            output.status.success(),
            "logging subprocess failed: {}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout)
        );
    }
}
