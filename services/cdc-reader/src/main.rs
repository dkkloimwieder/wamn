//! Run change capture from one mounted JSON configuration.

use std::path::PathBuf;

use anyhow::Context as _;
use clap::Parser;
use wamn_event::reader::ReaderConfig;

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
    database: wamn_postgres::ConnectionConfig,
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
    wamn_cdc_reader::run(ReaderConfig {
        org: settings.org,
        project: settings.project,
        env: settings.env,
        publication: settings.cdc_name.clone(),
        slot: settings.cdc_name,
        stream,
        cdc_url: settings.database.to_json()?,
        nats_url: settings.nats_url,
        nats_tls_ca: Some(wamn_broker::nats::CA_FILE.into()),
        nats_creds: Some(settings.nats_creds),
        feedback_secs: 5,
        stall_threshold_secs: 30,
        slot_poll_secs: 30,
        slot_safe_wal_warn_bytes: 256 << 20,
    })
    .await
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
