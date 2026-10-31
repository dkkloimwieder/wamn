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
        /// System database connection with control authority.
        #[arg(long, env = "WAMN_SYSTEM_DATABASE_URL", hide_env_values = true)]
        database_url: String,
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
            wamn_control::integration::build(&repository, &code).await?
        }
        IntegrationCommand::Publish {
            code,
            repository,
            registry,
            database_url,
            version,
            parameter_schema,
            operations,
            write_interfaces,
        } => {
            let built = wamn_control::integration::build(&repository, &code).await?;
            let publication = wamn_control::integration::Publication {
                version,
                parameter_schema: serde_json::from_slice(&std::fs::read(parameter_schema)?)?,
                operations,
                write_interfaces,
            };
            wamn_control::integration::publish(
                &built,
                &registry.registry(),
                &database_url,
                &publication,
            )
            .await?;
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

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    #[derive(Debug, clap::Parser)]
    struct Probe {
        #[command(flatten)]
        integration: super::IntegrationArgs,
    }

    #[test]
    fn publish_accepts_repeatable_write_interfaces() {
        let parsed = Probe::try_parse_from([
            "probe",
            "publish",
            "modbus",
            "--artifact-base",
            "localhost/integrations",
            "--registry-auth-file",
            "/private/auth.json",
            "--database-url",
            "postgres://unused",
            "--version",
            "2",
            "--parameter-schema",
            "/private/schema.json",
            "--operations",
            "wamn:modbus/client@1.0.0",
            "--write-interface",
            "wamn:modbus/write@1.0.0",
            "--write-interface",
            "example:meter/write@1.0.0",
        ])
        .unwrap();
        let super::IntegrationCommand::Publish {
            write_interfaces, ..
        } = parsed.integration.command
        else {
            panic!("publish");
        };
        assert_eq!(
            write_interfaces,
            ["wamn:modbus/write@1.0.0", "example:meter/write@1.0.0"]
        );
    }
}
