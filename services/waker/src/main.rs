//! wamn-waker binary: parse args, init tracing, run the scale-to-zero wake loop.
//!
//! Thin over the lib (SR9), mirroring the dispatcher binary: the loop core lives
//! in `wamn_waker` so the wakeproof gate reuses its scale client.

use std::str::FromStr as _;

use clap::Parser;

#[derive(Parser)]
#[command(name = "wamn-waker", version, about)]
struct Cli {
    /// Log level (the chart passes this before the service flags)
    #[arg(long = "log-level", global = true, default_value = "info")]
    log_level: String,

    #[command(flatten)]
    args: wamn_waker::WakeArgs,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let level = tracing::Level::from_str(&cli.log_level)
        .map_err(|_| anyhow::anyhow!("invalid log level: {}", cli.log_level))?;
    // Same shape as the dispatcher's no-OTEL path: stderr fmt layer, RUST_LOG
    // overriding --log-level.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level.as_str()));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    wamn_waker::run(cli.args).await
}
