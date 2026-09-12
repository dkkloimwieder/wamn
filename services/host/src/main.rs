//! wamn-host: the production host binary.
//!
//! MVP outcome: crash floor · M0 execution · flow composition.
//!
//! `host` — ClusterHost driven by the runtime-operator over NATS. The
//! long-lived services live in their own artifacts (SR9): `wamn-dispatcher`,
//! `wamn-run-worker`, `wamn-cdc-reader`.
//!
//! The one-shot control-plane verbs (provision*, apply-package, publish/promote,
//! dump/restore/copy-project-env, enable-cdc-project-env) live in `wamn-ctl`
//! (SR9); this artifact ships none of them.
//!
//! The test suite lives in the separate
//! `wamn-gates` binary (docs/operations/build-and-test.md); this artifact ships
//! none of it.

use std::str::FromStr as _;

use clap::{Parser, Subcommand};

mod host;

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
    /// Run the operator-managed wasmCloud host.
    Host(Box<host::HostArgs>),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .event_interval(1)
        .enable_all()
        .build()?;
    let result = runtime.block_on(async_main(cli));
    runtime.shutdown_timeout(wamn_runtime::lifecycle::RUNTIME_SHUTDOWN_BUDGET);
    result
}

async fn async_main(cli: Cli) -> anyhow::Result<()> {
    let level = tracing::Level::from_str(&cli.log_level)
        .map_err(|_| anyhow::anyhow!("invalid log level: {}", cli.log_level))?;
    // OTel exporters activate when OTEL_* env vars are present.
    let _shutdown_observability =
        wash_runtime::observability::initialize_observability(level, false, false)?;

    let result = match cli.command {
        Command::Host(args) => host::run(*args).await,
    };

    wamn_runtime::lifecycle::finish(result).await
}
