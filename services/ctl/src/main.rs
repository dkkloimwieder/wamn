//! Internal bootstrap worker. User commands live in wamn.

use clap::{Parser, Subcommand};
use wamn_ctl::{identity_verbs, platform_verbs, provisioning_verbs};

#[derive(Parser)]
#[command(name = "wamn-ctl", hide = true)]
struct Cli {
    #[arg(long, global = true, default_value = "info")]
    log_level: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(hide = true)]
    ProvisionSystem(provisioning_verbs::ProvisionSystemArgs),
    #[command(hide = true)]
    ProvisionOrg(provisioning_verbs::ProvisionOrgArgs),
    #[command(hide = true)]
    ProvisionIdentityIssuer(identity_verbs::IdentityIssuerArgs),
    #[command(hide = true)]
    Invite(identity_verbs::InviteArgs),
    #[command(subcommand, hide = true)]
    SystemSchema(platform_verbs::SystemSchemaCommand),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| cli.log_level.into()),
        )
        .with_writer(std::io::stderr)
        .log_internal_errors(false)
        .init();
    match cli.command {
        Command::ProvisionSystem(args) => provisioning_verbs::provision_system(args).await,
        Command::ProvisionOrg(args) => provisioning_verbs::provision_org(args).await,
        Command::ProvisionIdentityIssuer(args) => identity_verbs::provision_issuer(args).await,
        Command::Invite(args) => identity_verbs::invite(args).await,
        Command::SystemSchema(command) => {
            platform_verbs::run_system_schema(command, std::io::stdout().lock()).await
        }
    }
}
