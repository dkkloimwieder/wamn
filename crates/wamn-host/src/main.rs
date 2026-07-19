//! wamn-host: the production host binary.
//!
//! `host`            — ClusterHost driven by the runtime-operator over NATS.
//! `dispatch`        — the shared trigger dispatcher (cron + outbox + wakes).
//!
//! The one-shot control-plane verbs (provision*, publish/migrate-catalog,
//! dump/restore/copy-project-env, enable-cdc-project-env) live in `wamn-ctl`
//! (SR9); this artifact ships none of them.
//!
//! The gate suite (bench/pgbench/…/f1proof) lives in the separate
//! `wamn-gates` binary (docs/archive/structure-review.md SR1); this artifact ships
//! none of it.

use std::str::FromStr as _;

use clap::{Parser, Subcommand};
use wamn_host::{dispatch, event_reader, host, run_worker};

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
    /// Run the shared trigger dispatcher (5.14): cron + outbox + parked-wake across all projects
    Dispatch(dispatch::DispatchArgs),
    /// Run the production flow runner (5.14): claim from run_queue + drive the flowrunner, looping (fqg.8)
    RunWorker(run_worker::RunWorkerArgs),
    /// Run the CDC event reader for ONE project-env: walsender session → envelopes → the EVT_ JetStream stream, LSN advances only on ack (wamn-l5i9.10, D19 v3 §4)
    EventReader(event_reader::EventReaderArgs),
}

fn main() -> anyhow::Result<()> {
    wamn_host::advertise_memory_ceiling();
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
        Command::Dispatch(args) => dispatch::run(args).await,
        Command::RunWorker(args) => run_worker::run(args).await,
        Command::EventReader(args) => event_reader::run(args).await,
    };

    shutdown_observability();
    result
}
