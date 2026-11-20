//! Native control and its private operation worker.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
struct Cli {
    #[arg(long)]
    settings: Option<PathBuf>,
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
        #[arg(long)]
        max_projects: std::num::NonZeroU32,
    },
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
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

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
            cli.settings
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("--settings is required"))?,
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
        Command::ProvisionSystem {
            database,
            platform_domain,
            max_projects,
        } => {
            let request = wamn_platform_install::provision_system::ProvisionSystemRequest {
                database: wamn_postgres::ConnectionConfig::read(&database).await?,
                platform_domain,
                max_projects,
            };
            wamn_platform_install::provision_system::provision_system(&request).await
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
