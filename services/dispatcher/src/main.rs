//! wamn-dispatcher: the shared trigger dispatcher service binary (SR9).
//!
//! Pre-split this ran as `wamn-host dispatch`; the flags are unchanged, the
//! `dispatch` subcommand literal is gone (single-purpose binary).

use std::str::FromStr as _;

use anyhow::Context as _;
use clap::Parser;
use opentelemetry_sdk::metrics::SdkMeterProvider;

#[derive(Parser)]
#[command(name = "wamn-dispatcher", version, about)]
struct Cli {
    /// Log level (the chart passes this before the service flags)
    #[arg(long = "log-level", global = true, default_value = "info")]
    log_level: String,

    #[command(flatten)]
    args: wamn_dispatcher::DispatchArgs,
}

/// [9.8] The dispatcher's OWN minimal metrics provider — it is the one service
/// artifact that links no runtime (SR9), so it cannot reuse the fork's global
/// `SdkMeterProvider`. This mirrors `observability.rs`'s
/// MetricExporter -> SdkMeterProvider -> set_meter_provider, gated on `OTEL_*`
/// exactly like the fork (no env = no exporter, the gauge stays a no-op). The
/// periodic reader honors `OTEL_METRIC_EXPORT_INTERVAL` (default 60s — set low in
/// the manifests so the gate does not wait a minute).
fn init_metrics() -> anyhow::Result<Option<SdkMeterProvider>> {
    if !std::env::vars().any(|(k, _)| k.starts_with("OTEL_")) {
        return Ok(None);
    }
    let exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_tonic()
        .build()
        .context("build OTLP metric exporter (grpc-tonic)")?;
    let resource = opentelemetry_sdk::Resource::builder()
        .with_attribute(opentelemetry::KeyValue::new(
            "service.name",
            env!("CARGO_PKG_NAME"),
        ))
        .build();
    let provider = SdkMeterProvider::builder()
        .with_periodic_exporter(exporter)
        .with_resource(resource)
        .build();
    opentelemetry::global::set_meter_provider(provider.clone());
    Ok(Some(provider))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let level = tracing::Level::from_str(&cli.log_level)
        .map_err(|_| anyhow::anyhow!("invalid log level: {}", cli.log_level))?;
    // Same shape as the pre-split no-OTEL path of
    // wash_runtime::observability::initialize_observability: stderr fmt layer,
    // RUST_LOG overriding --log-level.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level.as_str()));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    // [9.8] the run-queue-depth gauge's provider (OTEL_*-gated); flushed on exit.
    let meter_provider = init_metrics()?;

    let result = wamn_dispatcher::run(cli.args).await;

    if let Some(provider) = meter_provider
        && let Err(e) = provider.shutdown()
    {
        eprintln!("failed to shut down meter provider: {e}");
    }
    result
}
