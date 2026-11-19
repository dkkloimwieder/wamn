//! Local integration builds and project administration.

use std::path::PathBuf;

/// Build a wasm integration or manage a project integration.
#[derive(Debug, clap::Args)]
pub struct IntegrationArgs {
    #[command(subcommand)]
    command: IntegrationCommand,
}

#[derive(Debug, clap::Subcommand)]
enum IntegrationCommand {
    /// Build a local integration artifact.
    Build {
        code: wamn_registry::integration_code::IntegrationCode,
        /// Repository that contains integrations/<code>.
        #[arg(long, default_value = ".")]
        repository: PathBuf,
    },
    List(crate::component_verbs::ConfigurationArgs),
    Get(crate::component_verbs::ConfigurationArgs),
    Disable(crate::component_verbs::ConfigurationArgs),
    Enable(crate::component_verbs::ConfigurationArgs),
    Delete(crate::component_verbs::ConfigurationArgs),
}

/// Run the control operation and print the immutable artifact digest.
pub async fn run(args: IntegrationArgs) -> anyhow::Result<()> {
    let built = match args.command {
        IntegrationCommand::Build { code, repository } => {
            wamn_publish::integration::build(&repository, &code).await?
        }
        IntegrationCommand::List(args) => {
            return crate::component_verbs::configuration(args, "integration.list").await;
        }
        IntegrationCommand::Get(args) => {
            return crate::component_verbs::configuration(args, "integration.get").await;
        }
        IntegrationCommand::Disable(args) => {
            return crate::component_verbs::configuration(args, "integration.disable").await;
        }
        IntegrationCommand::Enable(args) => {
            return crate::component_verbs::configuration(args, "integration.enable").await;
        }
        IntegrationCommand::Delete(args) => {
            return crate::component_verbs::configuration(args, "integration.delete").await;
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
