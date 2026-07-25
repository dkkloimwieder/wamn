//! wamn-dispatcher: the shared trigger dispatcher service binary (SR9).
//!
//! Pre-split this ran as `wamn-host dispatch`; the flags are unchanged, the
//! `dispatch` subcommand literal is gone (single-purpose binary).

use std::str::FromStr as _;

use anyhow::Context as _;
use clap::Parser;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::{
    EnvFilter, Layer as _, Registry, layer::SubscriberExt as _, util::SubscriberInitExt as _,
};

#[derive(Parser)]
#[command(name = "wamn-dispatcher", version, about)]
struct Cli {
    /// Log level (the chart passes this before the service flags)
    #[arg(long = "log-level", global = true, default_value = "info")]
    log_level: String,

    #[command(flatten)]
    args: wamn_dispatcher::DispatchArgs,
}

/// The dispatcher's minimal OTel providers — it is the one service
/// artifact that links no runtime (SR9), so it cannot reuse the fork's global
/// providers. Traces and metrics share the same `OTEL_*` activation and
/// shutdown discipline as `wash_runtime::observability`; no OTel environment
/// leaves only stderr formatting installed.
struct Providers {
    meter: Option<SdkMeterProvider>,
    tracer: Option<SdkTracerProvider>,
}

fn init_observability(level: tracing::Level) -> anyhow::Result<Providers> {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level.as_str()));
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(filter);
    if !std::env::vars().any(|(key, _)| key.starts_with("OTEL_")) {
        Registry::default().with(fmt_layer).init();
        return Ok(Providers {
            meter: None,
            tracer: None,
        });
    }

    let resource = opentelemetry_sdk::Resource::builder()
        .with_attribute(opentelemetry::KeyValue::new(
            "service.name",
            env!("CARGO_PKG_NAME"),
        ))
        .build();
    let span_exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .build()
        .context("build OTLP span exporter (grpc-tonic)")?;
    let tracer = SdkTracerProvider::builder()
        .with_batch_exporter(span_exporter)
        .with_resource(resource.clone())
        .build();
    let trace_layer = tracing_opentelemetry::layer()
        .with_tracer(tracer.tracer("wamn-dispatcher"))
        .with_filter(EnvFilter::new(level.as_str()));
    Registry::default().with(fmt_layer).with(trace_layer).init();

    let metric_exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_tonic()
        .build()
        .context("build OTLP metric exporter (grpc-tonic)")?;
    let meter = SdkMeterProvider::builder()
        .with_periodic_exporter(metric_exporter)
        .with_resource(resource.clone())
        .build();
    opentelemetry::global::set_meter_provider(meter.clone());
    Ok(Providers {
        meter: Some(meter),
        tracer: Some(tracer),
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let level = tracing::Level::from_str(&cli.log_level)
        .map_err(|_| anyhow::anyhow!("invalid log level: {}", cli.log_level))?;
    let providers = init_observability(level)?;

    let result = wamn_dispatcher::run(cli.args).await;

    if let Some(provider) = providers.tracer
        && let Err(e) = provider.shutdown()
    {
        eprintln!("failed to shut down tracer provider: {e}");
    }
    if let Some(provider) = providers.meter
        && let Err(e) = provider.shutdown()
    {
        eprintln!("failed to shut down meter provider: {e}");
    }
    result
}
