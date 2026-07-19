//! wamn-dispatcher: the shared trigger dispatcher service binary (SR9).
//!
//! Pre-split this ran as `wamn-host dispatch`; the flags are unchanged, the
//! `dispatch` subcommand literal is gone (single-purpose binary).

use std::str::FromStr as _;

use clap::Parser;

#[derive(Parser)]
#[command(name = "wamn-dispatcher", version, about)]
struct Cli {
    /// Log level (the chart passes this before the service flags)
    #[arg(long = "log-level", global = true, default_value = "info")]
    log_level: String,

    #[command(flatten)]
    args: wamn_dispatcher::DispatchArgs,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let level = tracing::Level::from_str(&cli.log_level)
        .map_err(|_| anyhow::anyhow!("invalid log level: {}", cli.log_level))?;
    // Same shape as the pre-split no-OTEL path of
    // wash_runtime::observability::initialize_observability: stderr fmt layer,
    // RUST_LOG overriding --log-level.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level.as_str()));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    wamn_dispatcher::run(cli.args).await
}
