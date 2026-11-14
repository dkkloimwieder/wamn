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
    BootstrapProject {
        #[arg(long)]
        connection: String,
    },
    #[command(hide = true)]
    Operation(wamn_control::Operation),
    #[command(hide = true)]
    Platform { request: String },
    #[command(hide = true)]
    PruneRecordHistory(wamn_control::retention::Target),
    #[command(hide = true)]
    PruneRunHistory(wamn_control::retention::Runs),
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
    if let Some(Command::BootstrapProject { connection }) = &cli.command {
        return wamn_platform_install::environment::initialize_database(connection).await;
    }
    let config = wamn_control::Configuration::read(
        cli.config
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("--config is required"))?,
    )?;
    match cli.command {
        Some(Command::BootstrapProject { .. }) => {
            unreachable!("bootstrap returned before component startup")
        }
        None => wamn_control::serve(config).await,
        Some(Command::Platform { request }) => {
            println!(
                "{}",
                wamn_control::platform::execute(&config, &request).await?
            );
            Ok(())
        }
        Some(Command::PruneRecordHistory(target)) => {
            wamn_control::retention::records(config, target).await
        }
        Some(Command::PruneRunHistory(args)) => wamn_control::retention::runs(config, args).await,
        Some(Command::Operation(operation)) => {
            println!("{}", wamn_control::execute(&config, operation).await?);
            Ok(())
        }
    }
}
