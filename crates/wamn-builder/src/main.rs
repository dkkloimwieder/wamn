//! wamn-builder: the one-shot custom-node build service (5.5).
//!
//! ctl-verb semantics (mirrors `wamn-ctl`): each verb is a module with a clap
//! `Args` and an async `run() -> anyhow::Result<()>`. Its OWN image (a
//! cargo-ful build sandbox), NOT the slim cargo-less `wamn-ctl` image.

use std::str::FromStr as _;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "wamn-builder", version, about)]
struct Cli {
    /// Log level (the Job passes this before the subcommand).
    #[arg(long = "log-level", global = true, default_value = "info")]
    log_level: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Build a custom node (cargo|jco) into a wasm component, screened through the 5.5 import lint
    Build(wamn_builder::build::BuildArgs),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let level = tracing::Level::from_str(&cli.log_level)
        .map_err(|_| anyhow::anyhow!("invalid log level: {}", cli.log_level))?;
    // The verbs report via stdout; this carries dep diagnostics to stderr.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level.as_str()));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    match cli.command {
        Command::Build(args) => wamn_builder::build::run(args).await,
    }
}
