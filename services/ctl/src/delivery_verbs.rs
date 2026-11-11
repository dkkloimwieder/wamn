//! Arguments of the delivery verbs.

use std::path::PathBuf;

use anyhow::Context as _;
use clap::Args;

/// Build selected platform images, then push them, and stamp their digests into a
/// copy of the release chart's defaults; a failed build pushes nothing
/// (docs/architecture/overview.md#environment-lifecycle).
#[derive(Debug, Args)]
pub struct BuildPlatformArgs {
    /// The `<host>[:<port>]` of the registry the images are pushed to. A
    /// loopback registry is reached over plain HTTP.
    #[arg(long)]
    pub registry: String,
    /// The directory the stamped release chart is written to. It must not exist.
    #[arg(long)]
    pub out: PathBuf,
    /// The checkout whose `Dockerfile` and release chart are built.
    #[arg(long, default_value = ".")]
    pub repository: PathBuf,
    /// Build and push nothing: stamp the registry's digests of the chart
    /// version's images, and refuse when any is missing (R15).
    #[arg(long, default_value_t = false)]
    pub stamp_only: bool,
    /// Select platform artifacts and optional UI from this cluster document.
    /// GCP also supplies Cloud Build inputs.
    #[arg(long)]
    pub cluster: Option<PathBuf>,
    /// Build with Cloud Build even when no other build runs. Requires
    /// `--cluster`, and the daily cap still applies.
    #[arg(long, default_value_t = false, conflicts_with = "stamp_only")]
    pub cloud: bool,
}

/// Build the platform, or with `--stamp-only` read it from the registry, and
/// print the stamped chart's directory.
pub async fn build_platform(args: BuildPlatformArgs) -> anyhow::Result<()> {
    use wamn_environment_control::platform_build::{
        BuildPlatformRequest, build_platform, stamp_only,
    };
    let repository = std::fs::canonicalize(&args.repository)
        .with_context(|| format!("find {}", args.repository.display()))?;
    let request = BuildPlatformRequest {
        cluster: args
            .cluster
            .as_deref()
            .map(wamn_environment_control::platform::document::ClusterDocument::compile)
            .transpose()?,
        version: wamn_release_control::release_chart::chart_version(
            &repository.join("deploy/platform/release"),
        )?,
        repository,
        registry: args.registry,
        out: args.out,
        cloud: args.cloud,
    };
    let chart = if args.stamp_only {
        stamp_only(&request).await?
    } else {
        build_platform(&request).await?
    };
    println!("{}", chart.display());
    Ok(())
}
