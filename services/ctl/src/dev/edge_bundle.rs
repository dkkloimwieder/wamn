//! Command-line inputs and output for the edge release bundle.

use std::path::PathBuf;

use anyhow::Context as _;

#[derive(Debug, clap::Args)]
pub struct DevEdgeBundleArgs {
    /// The `dev.json` of the environment whose loop published the release.
    #[arg(long, value_name = "FILE")]
    config: PathBuf,
    /// The new directory to write the bundle into.
    #[arg(long, value_name = "DIRECTORY")]
    out: PathBuf,
}

/// Write the bundle of the last release that the loop published, with the
/// ingress guest that the loop serves, and print its digest.
pub fn run(args: &DevEdgeBundleArgs) -> anyhow::Result<()> {
    let bytes =
        std::fs::read(&args.config).with_context(|| format!("read {}", args.config.display()))?;
    let config = wamn_control::dev::config::parse_config(&bytes)?;
    let local = config.local_artifacts();
    let digest = wamn_control::dev::edge_bundle::write(
        &local.directory,
        &local.flow_http_component,
        &args.out,
    )?;
    println!("edge bundle {} digest={digest}", args.out.display());
    Ok(())
}
