//! Orchestration facade for retained proof inputs and repository fixtures.
//!
//! MVP outcome: proof floor.
//!
//! `wamn-gates` retains the routing needed by the MVP proof inputs while their
//! implementations live in explicit conformance, integration, system, and
//! test-support homes. Proofs that import service clients remain integration
//! evidence even when they also exercise a deployed endpoint.

// Each proof implementation is owned and compiled by its tier package. This
// binary is only the stable deploy-facing command router.
use wamn_proof_conformance::socketguard;
use wamn_proof_integration::{capturebench, dashproof, m1, metricbench, readerbench, runnerbench};
use wamn_proof_system::traceproof;

use std::str::FromStr as _;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "wamn-gates", version, about)]
struct Cli {
    /// Log level (the Jobs pass this before the subcommand)
    #[arg(long = "log-level", global = true, default_value = "info")]
    log_level: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Prove admitted full/off node I/O capture, oversized-output metadata, redaction, and retention.
    Capturebench(capturebench::CaptureBenchArgs),
    /// Prove host-owned global-FIFO claim handoff without invoking the fail-closed guest interpreter.
    Runnerbench(runnerbench::RunnerBenchArgs),
    /// Assert an EVT_ stream holds a CDC reader's exact write program (order / dedupe / envelope shape) — the l5i9.10 gate's stream-side step
    Readerbench(readerbench::ReaderBenchArgs),
    /// Run M1 checks 9 and 10 over one isolated event-path fixture.
    M1(m1::M1Args),
    /// Idempotently clean resources owned by one exact M1 Job identity.
    M1Cleanup(m1::M1Args),
    /// Serve the 9.2 reflecting upstream (echoes received trace headers as JSON)
    ServeEcho(traceproof::ServeEchoArgs),
    /// Run the E13a publish-time egress-guard refusal gate (a wasi:sockets importer is refused; a standard component publishes)
    Socketguard(socketguard::SocketGuardArgs),
    /// Run the 9.2 trace-inject gate: prove the host stamps `traceparent` on both the P2 and P3 outbound surfaces, read back from serve-echo
    Traceproof(traceproof::TraceproofArgs),
    /// Run the 9.8 metric-set gate: drive the production emission seams and assert each `wamn_*` family lands in the collector's Prometheus scrape
    Metricbench(metricbench::MetricBenchArgs),
    /// Run the 9.9 dashboards gate: assert a deployed Grafana's health, its datasources, and the static plus per-tenant folders and dashboards
    Dashproof(dashproof::DashproofArgs),
}

fn main() -> anyhow::Result<()> {
    // The bench harnesses create stores through the same fork limiter the prod
    // host does; advertise the ceiling exactly like the prod binary.
    wamn_runtime::advertise_memory_ceiling();
    async_main()
}

#[tokio::main]
async fn async_main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let level = tracing::Level::from_str(&cli.log_level)
        .map_err(|_| anyhow::anyhow!("invalid log level: {}", cli.log_level))?;
    // OTel exporters activate when OTEL_* env vars are present.
    let shutdown_observability =
        wash_runtime::observability::initialize_observability(level, false, false)?;

    let result = match cli.command {
        Command::Capturebench(args) => capturebench::run(args).await,
        Command::Runnerbench(args) => runnerbench::run(args).await,
        Command::Readerbench(args) => readerbench::run(args).await,
        Command::M1(args) => m1::run(args).await,
        Command::M1Cleanup(args) => m1::cleanup(args).await,
        Command::ServeEcho(args) => traceproof::serve_echo(args).await,
        Command::Socketguard(args) => socketguard::run(args).await,
        Command::Traceproof(args) => traceproof::run(args).await,
        Command::Metricbench(args) => metricbench::run(args).await,
        Command::Dashproof(args) => dashproof::run(args).await,
    };

    shutdown_observability();
    result
}
