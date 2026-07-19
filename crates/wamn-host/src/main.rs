//! wamn-host: the production host binary.
//!
//! `host` — ClusterHost driven by the runtime-operator over NATS. The
//! long-lived services live in their own artifacts (SR9): `wamn-dispatcher`,
//! `wamn-run-worker`, `wamn-cdc-reader`.
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
use wamn_host::host;

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
    };

    shutdown_observability();
    result
}
