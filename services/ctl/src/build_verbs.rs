//! `wamn build`: the two-pass package build (docs/plan/platform-deploy.md §7.1).

use std::path::PathBuf;

use wamn_schema_generator::build::{
    BuildRequest, build_package, default_output_root, package_output,
};

/// Arguments of `wamn build`.
#[derive(Debug, clap::Args)]
pub struct BuildArgs {
    /// The package directories, built in the order given. Give a base package
    /// before its overlays: an overlay reads the base's build.json.
    #[arg(required = true)]
    pub packages: Vec<PathBuf>,
    /// The directory that holds one build output directory per package.
    /// Defaults to <package>/../target/wamn.
    #[arg(long)]
    pub output_root: Option<PathBuf>,
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
        let output_root = args
            .output_root
            .clone()
            .unwrap_or_else(|| default_output_root(package));
        let receipt = build_package(&BuildRequest::new(
            package,
            &output_root,
            &args.database_url,
        ))
        .await?;
        println!(
            "{} {} {} {}",
            receipt.package.id,
            receipt.package.version,
            receipt.outputs.manifest,
            package_output(&output_root, &receipt.package.id).display()
        );
    }
    Ok(())
}
