//! Integration artifact publication and project administration.

use std::path::PathBuf;

/// Build or publish one wasm integration.
#[derive(Debug, clap::Args)]
pub struct IntegrationArgs {
    #[command(subcommand)]
    command: IntegrationCommand,
}

#[derive(Debug, clap::Subcommand)]
enum IntegrationCommand {
    /// Build a local integration artifact.
    Build {
        code: String,
        /// Repository that contains integrations/<code>.
        #[arg(long, default_value = ".")]
        repository: PathBuf,
    },
    /// Build, push by digest, and record an integration version.
    Publish {
        code: String,
        /// Repository that contains integrations/<code>.
        #[arg(long, default_value = ".")]
        repository: PathBuf,
        #[command(flatten)]
        registry: crate::package_verbs::RegistryArgs,
        #[arg(long)]
        version: i32,
        #[arg(long)]
        parameter_schema: PathBuf,
        #[arg(long)]
        operations: String,
        /// Exported interface that permits writes. Repeat for each interface.
        #[arg(long = "write-interface")]
        write_interfaces: Vec<String>,
    },
    Write(crate::component_verbs::ConfigurationArgs),
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
        IntegrationCommand::Publish {
            code,
            repository,
            registry,
            version,
            parameter_schema,
            operations,
            write_interfaces,
        } => {
            let built = wamn_publish::integration::build(&repository, &code).await?;
            let publication = wamn_publish::integration::Publication {
                version,
                parameter_schema: serde_json::from_slice(&std::fs::read(parameter_schema)?)?,
                operations,
                write_interfaces,
            };
            wamn_publish::integration::push(&built, &registry.registry(), &publication).await?;
            crate::operation_verbs::ControlHost::from_env()?.call("integration.publish", &serde_json::json!({
                "code":built.code,"digest":built.digest,"version":publication.version,
                "parameter_schema":publication.parameter_schema,"operations":publication.operations,"write_interfaces":publication.write_interfaces,
            })).await?;
            built
        }
        IntegrationCommand::Write(args) => {
            return crate::component_verbs::configuration(args, "integration.write").await;
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
