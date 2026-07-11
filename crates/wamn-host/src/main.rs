//! wamn-host: custom wasmCloud host image (S1 PoC).
//!
//! `host`  — ClusterHost driven by the runtime-operator over NATS.
//! `bench` — S1 measurements (instantiation / density / cap-kill).

mod bench;
mod egressbench;
mod engine;
mod flowbench;
mod host;
mod logbench;
mod nodebench;
mod pgbench;
mod plugins;
mod testhostbench;

use std::str::FromStr as _;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "wamn-host", version, about)]
struct Cli {
    /// Log level (the chart passes this before the subcommand)
    #[arg(long = "log-level", global = true, default_value = "info")]
    log_level: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the wamn host (operator-managed via NATS)
    Host(Box<host::HostArgs>),
    /// Run the S1 benchmark suite
    Bench(bench::BenchArgs),
    /// Run the S2 wamn:postgres benchmark + security gates
    Pgbench(pgbench::PgBenchArgs),
    /// Run the S3 flow-runner gates (dispatch / hot-reload / resume)
    Flowbench(flowbench::FlowBenchArgs),
    /// Run the S4 custom-node gates (HTTP hop / interpreted-vs-composed / config parse)
    Nodebench(nodebench::NodeBenchArgs),
    /// Serve a wamn:node component over HTTP (S4 hop node host)
    ServeNode(nodebench::ServeNodeArgs),
    /// Run the S5 logging-capture gates (overhead / loss / drops / enrichment)
    Logbench(logbench::LogBenchArgs),
    /// Run the S6 test-host plugin-swap gates (sameness / delay / egress / regression)
    Testhostbench(testhostbench::TestHostBenchArgs),
    /// Run the 2.6 DB-path egress review gate (no shipped workload imports wasi:sockets)
    Egressbench(egressbench::EgressBenchArgs),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let level = tracing::Level::from_str(&cli.log_level)
        .map_err(|_| anyhow::anyhow!("invalid log level: {}", cli.log_level))?;
    // OTel exporters activate when OTEL_* env vars are present.
    let shutdown_observability =
        wash_runtime::observability::initialize_observability(level, false, false)?;

    let result = match cli.command {
        Command::Host(args) => host::run(*args).await,
        Command::Bench(args) => bench::run(args).await,
        Command::Pgbench(args) => pgbench::run(args).await,
        Command::Flowbench(args) => flowbench::run(args).await,
        Command::Nodebench(args) => nodebench::run(args).await,
        Command::ServeNode(args) => nodebench::serve(args).await,
        Command::Logbench(args) => logbench::run(args).await,
        Command::Testhostbench(args) => testhostbench::run(args).await,
        Command::Egressbench(args) => egressbench::run(args).await,
    };

    shutdown_observability();
    result
}
