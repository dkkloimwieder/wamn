//! wamn-run-worker: the production flow-runner service binary (SR9).
//!
//! Pre-split this ran as `wamn-host run-worker`; the flags are unchanged, the
//! `run-worker` subcommand literal is gone (single-purpose binary).

use std::str::FromStr as _;

use clap::Parser;

#[derive(Parser)]
#[command(name = "wamn-run-worker", version, about)]
struct Cli {
    /// Log level (the chart passes this before the service flags)
    #[arg(long = "log-level", global = true, default_value = "info")]
    log_level: String,

    #[command(flatten)]
    args: wamn_run_worker::RunWorkerArgs,
}

fn main() -> anyhow::Result<()> {
    // Before the tokio runtime exists — the fork's per-store limiter reads it.
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

    let result = wamn_run_worker::run(cli.args).await;

    shutdown_observability();
    result
}
