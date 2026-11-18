//! `wamn build`: the two-pass package build (docs/operations/building.md#wamn-build).

use std::path::PathBuf;

use wamn_build::{BuildRequest, DescribeMode, build_package, default_output_root, package_output};

/// Arguments of `wamn build`.
#[derive(Debug, clap::Args)]
pub struct BuildArgs {
    /// The package directories, built in the order given. Give a base package
    /// before its overlays.
    #[arg(required = true)]
    pub packages: Vec<PathBuf>,
    #[command(flatten)]
    describe: RefreshDescribeArgs,
    /// Compare the committed SQL describe evidence with PostgreSQL.
    #[arg(long, requires = "database_url", conflicts_with = "refresh_describe")]
    check_describe: bool,
}

#[derive(Debug, Default, clap::Args)]
pub(crate) struct RefreshDescribeArgs {
    /// PostgreSQL superuser URL for a disposable verification database.
    #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
    database_url: Option<String>,
    /// Recreate the committed SQL describe evidence from PostgreSQL.
    #[arg(long, requires = "database_url")]
    pub(crate) refresh_describe: bool,
}

impl RefreshDescribeArgs {
    pub(crate) fn mode(&self, check: bool) -> anyhow::Result<DescribeMode<'_>> {
        DescribeMode::from_options(self.database_url.as_deref(), self.refresh_describe, check)
    }
}

/// Build each package and print its manifest digest and output directory.
///
/// # Errors
///
/// When a build fails. The packages before it keep their output.
pub async fn run(args: BuildArgs) -> anyhow::Result<()> {
    let describe = args.describe.mode(args.check_describe)?;
    for package in &args.packages {
        let output_root = default_output_root(package);
        let built = build_package(&BuildRequest::new(package, &output_root, describe)).await?;
        println!(
            "{} {} {} {}",
            built.package.id,
            built.package.version,
            built.manifest_digest,
            package_output(&output_root, &built.package.id).display()
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
        name: wamn_catalog::manifest::source_name::SourceName,
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
