//! wamn-host: custom wasmCloud host image (S1 PoC).
//!
//! `host`  — ClusterHost driven by the runtime-operator over NATS.
//! `bench` — S1 measurements (instantiation / density / cap-kill).

mod apibench;
mod apifixture;
mod apiproof;
mod bench;
mod dispatch;
mod dispatchbench;
mod egressbench;
mod engine;
mod f1bench;
mod f1fixture;
mod f1proof;
mod failoverbench;
mod flowbench;
mod host;
mod logbench;
mod nodebench;
mod pgbench;
mod plugins;
mod publish_catalog;
mod queuebench;
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
    /// Run the 5.14 durable-run-queue gates (dispatch SLOs / throughput / reclaim / janitor / doorbell)
    Queuebench(queuebench::QueueBenchArgs),
    /// Run the 5.14 failover gates (checkpoint/resume on replica loss / janitor completion-race guard)
    Failoverbench(failoverbench::FailoverBenchArgs),
    /// Run the shared trigger dispatcher (5.14): cron + outbox + parked-wake across all projects
    Dispatch(dispatch::DispatchArgs),
    /// Run the 5.14 dispatcher gates (cron / outbox / race / fairness / wake / live)
    Dispatchbench(dispatchbench::DispatchBenchArgs),
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
    /// Run the 4.1 generated-REST-API-gateway gates (CRUD / expand / RLS / injection)
    Apibench(apibench::ApiBenchArgs),
    /// Write a project's catalog snapshot into the wamn_catalog table (4.1b)
    PublishCatalog(publish_catalog::PublishCatalogArgs),
    /// Run the 4.1b in-cluster proof against a deployed api-gateway over HTTP
    Apiproof(apiproof::ApiProofArgs),
    /// Run the POC-F1 receipt-received gates (happy / holds / invalid / burst / rest)
    F1bench(f1bench::F1BenchArgs),
    /// Run the POC-F1 proof against the deployed poc-webhook-f1 + api-gateway over HTTP
    F1proof(f1proof::F1ProofArgs),
}

fn main() -> anyhow::Result<()> {
    // Advertise the platform memory ceiling to the fork's per-store limiter
    // (docs/wash-runtime-fork.md): a workload budget above this is a hard
    // store-creation error, never a silent clamp. SAFETY: set before the
    // tokio runtime exists — no other threads are reading the environment.
    unsafe {
        std::env::set_var(
            "WAMN_MEMORY_CEILING_MB",
            (engine::MEMORY_CAP_BYTES >> 20).to_string(),
        );
    }
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
        Command::Host(args) => host::run(*args).await,
        Command::Bench(args) => bench::run(args).await,
        Command::Pgbench(args) => pgbench::run(args).await,
        Command::Flowbench(args) => flowbench::run(args).await,
        Command::Queuebench(args) => queuebench::run(args).await,
        Command::Failoverbench(args) => failoverbench::run(args).await,
        Command::Dispatch(args) => dispatch::run(args).await,
        Command::Dispatchbench(args) => dispatchbench::run(args).await,
        Command::Nodebench(args) => nodebench::run(args).await,
        Command::ServeNode(args) => nodebench::serve(args).await,
        Command::Logbench(args) => logbench::run(args).await,
        Command::Testhostbench(args) => testhostbench::run(args).await,
        Command::Egressbench(args) => egressbench::run(args).await,
        Command::Apibench(args) => apibench::run(args).await,
        Command::PublishCatalog(args) => publish_catalog::run(args).await,
        Command::Apiproof(args) => apiproof::run(args).await,
        Command::F1bench(args) => f1bench::run(args).await,
        Command::F1proof(args) => f1proof::run(args).await,
    };

    shutdown_observability();
    result
}
