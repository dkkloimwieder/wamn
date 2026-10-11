//! Arguments of the delivery verbs.

use std::path::PathBuf;

use anyhow::Context as _;
use clap::Args;
use wamn_control::delivery::qualification;
use wamn_control::package_artifact::PackageRegistry;
use wamn_runtime::component_artifact_source::OCI_CA_PATHS_ENV;

/// Qualify one published release, by its digest, on the release chart's
/// platform image set through the existing application cases, and push the
/// qualification beside the release (docs/plan/platform-deploy.md §8.2).
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

/// Build and push the platform images and stamp their digests into a copy
/// of the release chart's defaults (docs/plan/platform-deploy.md §9.3, R15).
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
}

/// Run explicitly selected existing behavior checks before integration.
#[derive(Debug, Args)]
pub struct CheckChangesArgs {
    #[arg(long, default_value = ".")]
    pub repository: PathBuf,
    /// A member of the root or apps workspace.
    #[arg(long)]
    pub package: String,
    /// Omit for the library test target.
    #[arg(long)]
    pub test: Option<String>,
    #[arg(long = "case", required = true)]
    pub cases: Vec<String>,
    #[arg(long)]
    pub include_ignored: bool,
    #[arg(long)]
    pub result: PathBuf,
}

/// Qualify one published release by its digest, and print the
/// qualification's digest and whether this run pushed it.
pub async fn qualify(args: QualifyReleaseArgs) -> anyhow::Result<()> {
    let (digest, disposition) = qualification::qualify(qualification::QualifyReleaseRequest {
        release_digest: args.release_digest,
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

/// Build the platform and print the stamped chart's directory.
pub async fn build_platform(args: BuildPlatformArgs) -> anyhow::Result<()> {
    use wamn_control::platform_build::{BuildPlatformRequest, build_platform};
    let chart = build_platform(&BuildPlatformRequest {
        repository: std::fs::canonicalize(&args.repository)
            .with_context(|| format!("find {}", args.repository.display()))?,
        registry: args.registry,
        out: args.out,
    })
    .await?;
    println!("{}", chart.display());
    Ok(())
}

/// Take one installed environment to the bytes of one commit
/// (docs/plan/upgrade-environment.md).
#[derive(Debug, Args)]
pub struct UpgradeEnvironmentArgs {
    #[arg(long)]
    pub org: String,
    #[arg(long)]
    pub project: String,
    #[arg(long = "env")]
    pub environment: String,
    /// A full 40-hex commit.
    #[arg(long)]
    pub commit: String,
    /// The repository that holds the commit. The verb builds in its own checkout.
    #[arg(long, default_value = ".")]
    pub repository: PathBuf,
    /// The parent of the work directory of each commit. The default is
    /// `${XDG_CACHE_HOME:-$HOME/.cache}/wamn-upgrade`.
    #[arg(long)]
    pub work_root: Option<PathBuf>,
    /// The manifest digest of the release the environment serves now. An
    /// upgrade of an installed package needs it.
    #[arg(long, value_name = "DIGEST")]
    pub predecessor_release: Option<String>,
}

pub async fn upgrade_environment(args: UpgradeEnvironmentArgs) -> anyhow::Result<()> {
    use wamn_control::upgrade_environment::{UpgradeEnvironmentRequest, upgrade_environment};
    let work_root = match args.work_root {
        Some(root) => root,
        None => std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
            .context("neither XDG_CACHE_HOME nor HOME is set; pass --work-root")?
            .join("wamn-upgrade"),
    };
    let record = Box::pin(upgrade_environment(&UpgradeEnvironmentRequest {
        repository: std::fs::canonicalize(&args.repository)
            .with_context(|| format!("find {}", args.repository.display()))?,
        work_root,
        arguments: wamn_control::delivery::run_record::RunArguments::new(
            &args.org,
            &args.project,
            &args.environment,
            &args.commit,
        )?,
        predecessor_release: args.predecessor_release,
    }))
    .await?;
    println!("{}", serde_json::to_string_pretty(&record)?);
    Ok(())
}

/// Execute selected existing tests before integration.
pub async fn check_changes(args: CheckChangesArgs) -> anyhow::Result<()> {
    qualification::check_changes(qualification::CheckChangesRequest {
        repository: args.repository,
        package: args.package,
        test: args.test,
        cases: args.cases,
        include_ignored: args.include_ignored,
        result: args.result,
    })
    .await
}
