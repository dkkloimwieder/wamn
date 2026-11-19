//! Serve web traffic selected by ingress.
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
struct Cli {
    #[arg(long)]
    settings: PathBuf,
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Debug, Subcommand)]
enum Command {
    Probe,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(tracing_subscriber::filter::LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .log_internal_errors(false)
        .init();
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Probe) => wamn_web::probe(&cli.settings).await,
        None => wamn_web::serve(&cli.settings).await,
    }
}
