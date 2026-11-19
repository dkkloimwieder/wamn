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
    #[command(hide = true)]
    ProvisionOrg {
        org: wamn_registry::OrgId,
        #[arg(long)]
        database: PathBuf,
    },
    #[command(hide = true)]
    InviteOrgOwner {
        org: wamn_registry::OrgId,
        #[arg(long)]
        owner: String,
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
    let configuration = || {
        wamn_control::Configuration::read(
            cli.config
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("--config is required"))?,
        )
    };
    match cli.command {
        Command::Platform => wamn_control::serve(configuration()?).await,
        Command::Worker => {
            let config = configuration()?;
            let request = std::fs::read_to_string("/etc/wamn/request/request.json")?;
            println!(
                "{}",
                wamn_control::platform::execute(&config, &request).await?
            );
            Ok(())
        }
        Command::ProvisionOrg { org, database } => {
            let database = wamn_postgres::ConnectionConfig::read(&database).await?;
            wamn_platform_install::provision_org::provision_org(&database, &org).await
        }
        Command::InviteOrgOwner { org, owner } => {
            wamn_control::first_owner::invite(&org, &owner).await
        }
    }
}
