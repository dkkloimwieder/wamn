//! `wamn build`: the two-pass package build (docs/operations/building.md#wamn-build).

use std::path::PathBuf;

use wamn_build::{BuildRequest, build_package, default_output_root, package_output};

/// Arguments of `wamn build`.
#[derive(Debug, clap::Args)]
pub struct BuildArgs {
    /// The package directories, built in the order given. Give a base package
    /// before its overlays: an overlay reads the base's build.json.
    #[arg(required = true)]
    pub packages: Vec<PathBuf>,
    /// A superuser URL of a PostgreSQL server. The build creates its own
    /// verification database there and drops it at the end.
    #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
    pub database_url: String,
}

/// Build each package and print its manifest digest and output directory.
///
/// # Errors
///
/// When a build fails. The packages before it keep their output.
pub async fn run(args: BuildArgs) -> anyhow::Result<()> {
    for package in &args.packages {
        let output_root = default_output_root(package);
        let build_record = build_package(&BuildRequest::new(
            package,
            &output_root,
            &args.database_url,
        ))
        .await?;
        println!(
            "{} {} {} {}",
            build_record.package.id,
            build_record.package.version,
            build_record.outputs.manifest,
            package_output(&output_root, &build_record.package.id).display()
        );
    }
    Ok(())
}

/// Take ownership of one generated action or browser component.
#[derive(Debug, clap::Args)]
pub struct EjectArgs {
    #[command(subcommand)]
    pub target: EjectTarget,
}
/// The generated default to copy.
#[derive(Debug, clap::Subcommand)]
pub enum EjectTarget {
    /// Replace an empty Action implementation with its current default body.
    Action { package: PathBuf, operation: String },
    /// Copy a browser component to an authored file for composition.
    Component {
        package: PathBuf,
        name: String,
        output: PathBuf,
    },
}
/// Eject a generated default into application source.
pub fn eject(args: EjectArgs) -> anyhow::Result<()> {
    match args.target {
        EjectTarget::Action { package, operation } => {
            println!(
                "{}",
                wamn_build::eject::action(&package, &operation)?.display()
            );
        }
        EjectTarget::Component {
            package,
            name,
            output,
        } => {
            wamn_build::eject::component(&package, &name, &output)?;
            println!("{}", output.display());
        }
    }
    Ok(())
}
