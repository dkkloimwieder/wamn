//! Native control and its private operation worker.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

use wamn_platform_install::{
    env_install::{self, EnvSqlStep},
    provision_org::{self, OrgActivity},
};

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
    },
    #[command(hide = true)]
    ProvisionOrg {
        org: wamn_registry::OrgId,
        #[arg(long)]
        database: PathBuf,
    },
    #[command(hide = true)]
    ProvisionEnv(ProvisionEnv),
    #[command(hide = true)]
    ProvisionEnvWorker(ProvisionEnv),
    #[command(hide = true)]
    ActivateOrg {
        org: wamn_registry::OrgId,
        #[arg(long)]
        database: PathBuf,
    },
    #[command(hide = true)]
    DeactivateOrg {
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

#[derive(clap::Args)]
struct ProvisionEnv {
    org: wamn_registry::OrgId,
    env: wamn_registry::EnvName,
    #[arg(long)]
    prod: bool,
    #[arg(long)]
    database: PathBuf,
}

impl ProvisionEnv {
    async fn run(&self, step: EnvSqlStep) -> anyhow::Result<()> {
        let default_cpu_millicores =
            env_install::read_env_default_cpu_millicores(&self.database).await?;
        let database = wamn_postgres::read_connection_config(&self.database)
            .await?
            .to_json()?;
        env_install::provision_in_cluster(
            &database,
            &self.org,
            &self.env,
            self.prod,
            default_cpu_millicores,
            step,
        )
        .await
    }
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
        } => {
            let request = wamn_platform_install::provision_system::ProvisionSystemRequest {
                database: wamn_postgres::read_connection_config(&database).await?,
                platform_domain,
            };
            wamn_platform_install::provision_system::provision_system(&request).await
        }
        Command::ProvisionOrg { org, database } => {
            let database = wamn_postgres::read_connection_config(&database)
                .await?
                .to_json()?;
            let setup =
                wamn_kubernetes::client::KubernetesApi::in_cluster().map_err(anyhow::Error::from);
            wamn_platform_install::provision_org::provision_org(&database, &org, setup).await
        }
        Command::ProvisionEnv(args) => args.run(EnvSqlStep::Record).await,
        Command::ProvisionEnvWorker(args) => args.run(EnvSqlStep::PublishWorker).await,
        Command::ActivateOrg { org, database } => {
            let database = wamn_postgres::read_connection_config(&database)
                .await?
                .to_json()?;
            provision_org::set_activity(&database, &org, OrgActivity::Active).await
        }
        Command::DeactivateOrg { org, database } => {
            let database = wamn_postgres::read_connection_config(&database)
                .await?
                .to_json()?;
            provision_org::set_activity(&database, &org, OrgActivity::Inactive).await
        }
        Command::InviteOrgOwner { org, owner } => {
            wamn_control::first_owner::invite(&org, &owner).await
        }
    }
}
