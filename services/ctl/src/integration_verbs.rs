//! Arguments and output for connector builds and publication.

use std::path::PathBuf;

/// Build or publish one wasm integration.
#[derive(Debug, clap::Args)]
pub struct IntegrationArgs {
    #[command(subcommand)]
    command: IntegrationCommand,
}

#[derive(Debug, clap::Subcommand)]
enum IntegrationCommand {
    /// Build a local connector artifact.
    Build {
        code: String,
        /// Repository that contains integrations/<code>.
        #[arg(long, default_value = ".")]
        repository: PathBuf,
    },
    /// Build, push by digest, and record a connector in the catalog.
    Publish {
        code: String,
        /// Repository that contains integrations/<code>.
        #[arg(long, default_value = ".")]
        repository: PathBuf,
        #[command(flatten)]
        registry: crate::package_verbs::RegistryArgs,
        /// System database connection with control authority.
        #[arg(long, env = "WAMN_SYSTEM_DATABASE_URL", hide_env_values = true)]
        database_url: String,
    },
}

/// Run the control operation and print the immutable artifact digest.
pub async fn run(args: IntegrationArgs) -> anyhow::Result<()> {
    let built = match args.command {
        IntegrationCommand::Build { code, repository } => {
            wamn_control::integration::build(&repository, &code).await?
        }
        IntegrationCommand::Publish {
            code,
            repository,
            registry,
            database_url,
        } => {
            let built = wamn_control::integration::build(&repository, &code).await?;
            wamn_control::integration::publish(&built, &registry.registry(), &database_url).await?;
            built
        }
    };
    println!(
        "{} {} {} runtime_type={}",
        built.code,
        built.digest,
        built.directory.display(),
        built.runtime_type,
    );
    Ok(())
}
