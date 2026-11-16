//! Native control and its private operation worker.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
struct Cli {
    #[arg(long)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    #[command(hide = true)]
    Platform { request: String },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .log_internal_errors(false)
        .init();
    let cli = Cli::parse();
    let config = wamn_control::Configuration::read(
        cli.config
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("--config is required"))?,
    )?;
    match cli.command {
        None => wamn_control::serve(config).await,
        Some(Command::Platform { request }) => {
            println!(
                "{}",
                wamn_control::platform::execute(&config, &request).await?
            );
            Ok(())
        }
    }
}
