//! Native control and its private operation worker.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
struct Cli {
    #[arg(long)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Platform,
    #[command(hide = true)]
    Worker,
    /// Install or upgrade the in-cluster system database.
    ProvisionSystem {
        #[arg(long)]
        database: PathBuf,
        #[arg(long)]
        platform_domain: wamn_registry::platform_domain::PlatformDomain,
    },
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
    if let Command::ProvisionSystem {
        database,
        platform_domain,
    } = cli.command
    {
        let request = wamn_platform_install::provision_system::ProvisionSystemRequest {
            database: wamn_postgres::ConnectionConfig::read(&database).await?,
            platform_domain,
        };
        return wamn_platform_install::provision_system::provision_system(&request).await;
    }
    let config = cli
        .config
        .ok_or_else(|| anyhow::anyhow!("--config is required"))?;
    let config = wamn_control::Configuration::read(&config)?;
    match cli.command {
        Command::Platform => wamn_control::serve(config).await,
        Command::Worker => {
            let request = std::fs::read_to_string("/etc/wamn/request/request.json")?;
            println!(
                "{}",
                wamn_control::platform::execute(&config, &request).await?
            );
            Ok(())
        }
        Command::ProvisionSystem { .. } => unreachable!("provision-system already dispatched"),
    }
}
