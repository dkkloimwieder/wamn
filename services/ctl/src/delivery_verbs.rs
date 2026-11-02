//! Arguments of the delivery verbs.

use std::path::PathBuf;

use crate::qualification;
use anyhow::Context as _;
use clap::Args;
use wamn_control::package_artifact::PackageRegistry;
use wamn_runtime::component_artifact_source::OCI_CA_PATHS_ENV;

/// Qualify one published release, by its digest, on the release chart's
/// platform image set through the existing application cases, and push the
/// qualification beside the release
/// (docs/architecture/overview.md#release-identity).
#[derive(Debug, Args)]
pub struct QualifyReleaseArgs {
    /// The published release, as its manifest digest.
    #[arg(long)]
    pub release_digest: String,
    /// The `<registry>/<repository>` that `publish-release` pushed the
    /// manifest to. The qualification is pushed beside it.
    #[arg(long)]
    pub artifact_base: String,
    /// `.dockerconfigjson` file carrying the registry credential.
    #[arg(long, env = "WAMN_REGISTRY_AUTH_FILE")]
    pub registry_auth_file: PathBuf,
    /// Use plain HTTP for exactly the registry in `--artifact-base`.
    #[arg(long, default_value_t = false)]
    pub insecure_registry: bool,
    /// PEM CA bundle trusted for the registry, on top of the compiled-in
    /// roots. Repeat or comma-delimit. Env `WASH_OCI_CA_PATHS`.
    #[arg(long = "oci-ca-path", env = OCI_CA_PATHS_ENV, value_delimiter = ',')]
    pub oci_ca_paths: Vec<PathBuf>,
    /// The stamped release chart whose default values name the platform
    /// image set the cases run on, and the gates, identity and runner images
    /// (R12, R15).
    #[arg(long, env = "WAMN_RELEASE_CHART")]
    pub release_chart: PathBuf,
}

/// Build every platform image, then push them, and stamp their digests into a
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
    /// Cluster document with the cloud build project and daily spend cap.
    #[arg(long)]
    pub cluster: Option<PathBuf>,
    /// Build with Cloud Build even when no other build runs. Requires
    /// `--cluster`, and the daily cap still applies.
    #[arg(long, default_value_t = false, conflicts_with = "stamp_only")]
    pub cloud: bool,
}

/// Qualify one published release by its digest, and print the
/// qualification's digest and whether this run pushed it, found it already
/// pushed, or reused it without running a case.
pub async fn qualify(args: QualifyReleaseArgs) -> anyhow::Result<()> {
    let (digest, disposition) = qualification::qualify(qualification::QualifyReleaseRequest {
        release_digest: args.release_digest,
        repository: repository(&std::env::current_dir()?)?,
        releases: PackageRegistry {
            artifact_base: args.artifact_base,
            registry_auth_file: args.registry_auth_file,
            insecure_registry: args.insecure_registry,
            oci_ca_paths: args.oci_ca_paths,
        },
        release_chart: args.release_chart,
    })
    .await?;
    println!("{digest} {disposition:?}");
    Ok(())
}

/// Build the platform, or with `--stamp-only` read it from the registry, and
/// print the stamped chart's directory.
pub async fn build_platform(args: BuildPlatformArgs) -> anyhow::Result<()> {
    use wamn_control::platform_build::{BuildPlatformRequest, build_platform, stamp_only};
    let repository = std::fs::canonicalize(&args.repository)
        .with_context(|| format!("find {}", args.repository.display()))?;
    let request = BuildPlatformRequest {
        cluster: args
            .cluster
            .as_deref()
            .map(wamn_control::platform::document::ClusterDocument::compile)
            .transpose()?,
        version: wamn_control::release_chart::chart_version(
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

/// Find the enclosing repository checkout for application declarations.
fn repository(directory: &std::path::Path) -> anyhow::Result<PathBuf> {
    directory
        .ancestors()
        .find(|path| path.join("apps/Cargo.toml").is_file() && path.join("Cargo.toml").is_file())
        .map(PathBuf::from)
        .context("qualify-release requires a repository checkout with apps/Cargo.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualification_requires_an_enclosing_checkout() {
        let checkout = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        assert_eq!(
            repository(&checkout.join("apps/wamn_receiving/tests")).unwrap(),
            checkout
        );
        assert!(
            repository(std::path::Path::new("/without-a-checkout"))
                .unwrap_err()
                .to_string()
                .contains("requires a repository checkout")
        );
    }
}
