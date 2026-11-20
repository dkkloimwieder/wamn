//! Local integration builds.

use std::path::PathBuf;

/// Build a WebAssembly integration.
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
}

/// Build the integration and print its artifact digest.
pub async fn run(args: IntegrationArgs) -> anyhow::Result<()> {
    let IntegrationCommand::Build { code, repository } = args.command;
    let built = wamn_publish::integration::build(&repository, &code).await?;
    println!(
        "{} {} {} runtime_type={}",
        built.code,
        built.digest,
        built.directory.display(),
        built.runtime_type,
    );
    Ok(())
}
