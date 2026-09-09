//! wamn-run-worker: the production serving executor binary.
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
    args: wamn_executor::ExecutorArgs,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread()
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

    let result = wamn_executor::run(cli.args).await;

    wamn_runtime::lifecycle::finish(result).await
}
