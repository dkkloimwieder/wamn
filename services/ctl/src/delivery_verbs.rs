//! Arguments and output of the release-environment and delivery verbs.
//!
//! # The host carrier uses explicit flags
//!
//! The host takes the release pair as `hostGroups[].extraArgs` flags. Clap
//! exits nonzero on an unknown flag, so a misspelt
//! `--release-manifest-diges` crashloops the pod, while a misspelt
//! `WAMN_RELEASE_MANIFEST_DIGES` would deploy cleanly and serve nothing.

use std::path::PathBuf;

use anyhow::Context as _;
use clap::Args;
use wamn_control::delivery::qualification;
use wamn_control::package_artifact::PackageRegistry;
use wamn_control::print_release_env::{ReleaseCarrier, lookup_release_carrier};
use wamn_runtime::component_artifact_source::OCI_CA_PATHS_ENV;

/// The host's carrier: per host group `extraArgs`, flags rather than env. The
/// verb reads only the release, so it names the field, not an overlay file:
/// each application and environment has its own host overlay.
const HOST_CARRIER: &str = "hostGroups[].extraArgs of the selected host overlay";

const ARTIFACT_BASE_FLAG: &str = "--release-artifact-base";
const MANIFEST_DIGEST_FLAG: &str = "--release-manifest-digest";

/// Arguments naming the published release whose lines are printed.
#[derive(Debug, Args)]
pub struct PrintReleaseEnvArgs {
    /// URL to the database holding the published release snapshot.
    #[arg(long)]
    pub database_url: String,

    /// Tenant claim carried by the published release snapshot.
    #[arg(long)]
    pub tenant: String,

    /// The published release, as its manifest digest.
    #[arg(long)]
    pub release_digest: String,

    /// The `<registry>/<repository>` the release manifest was pushed to.
    #[arg(long)]
    pub artifact_base: String,
}

/// Qualify one published release, by its digest, through the existing
/// application cases (docs/plan/platform-deploy.md §8.2).
#[derive(Debug, Args)]
pub struct QualifyReleaseArgs {
    #[arg(long, default_value = ".")]
    pub repository: PathBuf,
    /// Integrated main, a release tag, or an explicitly selected revision.
    #[arg(long, default_value = "main")]
    pub revision: String,
    /// The published release, as its manifest digest.
    #[arg(long)]
    pub release_digest: String,
    /// The `<registry>/<repository>` that `publish-release` pushed the
    /// manifest to.
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
    /// Org the cases provision and publish under.
    #[arg(long)]
    pub org: String,
    /// Project the cases provision and publish under.
    #[arg(long)]
    pub project: String,
    /// Environment the cases provision and publish under.
    #[arg(long)]
    pub environment: String,
    /// Tenant the cases provision and publish under.
    #[arg(long)]
    pub tenant: String,
    /// Route host the cases serve under.
    #[arg(long)]
    pub route_host: String,
    #[arg(long)]
    pub target_directory: PathBuf,
    #[arg(long)]
    pub host_image: String,
    /// The http role component the cases deploy through the release chart,
    /// `<repository>@sha256:<hex>`.
    #[arg(long)]
    pub http_image: String,
    /// The materializer role component, `<repository>@sha256:<hex>`.
    #[arg(long)]
    pub materializer_image: String,
    #[arg(long)]
    pub gates_image: Option<String>,
    #[arg(long)]
    pub identity_image: Option<String>,
    #[arg(long)]
    pub native_registry_endpoint: Option<String>,
    #[arg(long, requires = "native_registry_endpoint")]
    pub native_registry_insecure: bool,
    #[arg(long = "deployment-file")]
    pub deployment_files: Vec<PathBuf>,
    /// Fresh temporary result file outside tracked source.
    #[arg(long)]
    pub result: PathBuf,
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

/// Print the release lines for one published release.
pub async fn print_release_env(args: PrintReleaseEnvArgs) -> anyhow::Result<()> {
    let carrier = lookup_release_carrier(
        &args.database_url,
        &args.tenant,
        &args.release_digest,
        &args.artifact_base,
    )
    .await?;
    print!("{}", release_lines(&carrier));
    Ok(())
}

/// Render the release lines each carrier takes, labelled by carrier file.
fn release_lines(carrier: &ReleaseCarrier) -> String {
    let ReleaseCarrier {
        artifact_base,
        manifest_digest,
    } = carrier;
    format!(
        "# {HOST_CARRIER}\n\
         {ARTIFACT_BASE_FLAG}={artifact_base}\n\
         {MANIFEST_DIGEST_FLAG}={manifest_digest}\n"
    )
}

/// Qualify one published release by its digest.
pub async fn qualify(args: QualifyReleaseArgs) -> anyhow::Result<()> {
    qualification::qualify(qualification::QualifyReleaseRequest {
        repository: args.repository,
        revision: args.revision,
        release_digest: args.release_digest,
        releases: PackageRegistry {
            artifact_base: args.artifact_base,
            registry_auth_file: args.registry_auth_file,
            insecure_registry: args.insecure_registry,
            oci_ca_paths: args.oci_ca_paths,
            control_database_url: String::new(),
        },
        org: args.org,
        project: args.project,
        environment: args.environment,
        tenant: args.tenant,
        route_host: args.route_host,
        target_directory: args.target_directory,
        host_image: args.host_image,
        http_image: args.http_image,
        materializer_image: args.materializer_image,
        gates_image: args.gates_image,
        identity_image: args.identity_image,
        native_registry_endpoint: args.native_registry_endpoint,
        native_registry_insecure: args.native_registry_insecure,
        deployment_files: args.deployment_files,
        result: args.result,
    })
    .await
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

#[cfg(test)]
mod tests {
    use clap::Parser as _;
    use wamn_catalog::ManifestDigest;

    use super::*;

    /// Host command for the flattened argument surface under test.
    #[derive(Debug, clap::Parser)]
    struct PrintProbe {
        #[command(flatten)]
        args: PrintReleaseEnvArgs,
    }

    /// The published release the fixture coordinates name.
    const RELEASE: &str = "sha256:3333333333333333333333333333333333333333333333333333333333333333";

    const COORDINATE: [&str; 8] = [
        "--database-url",
        "postgres://release.invalid/env",
        "--tenant",
        "tenant-a",
        "--release-digest",
        RELEASE,
        "--artifact-base",
        "registry.example/wamn/releases",
    ];

    fn digest() -> ManifestDigest {
        ManifestDigest::parse(format!("sha256:{}", "7".repeat(64)))
            .expect("the fixture digest is canonical")
    }

    #[test]
    fn one_release_prints_both_carriers_and_nothing_else() {
        let printed = release_lines(&ReleaseCarrier {
            artifact_base: "registry.example/wamn/releases".to_owned(),
            manifest_digest: digest(),
        });
        assert_eq!(
            printed,
            format!(
                "# hostGroups[].extraArgs of the selected host overlay\n\
                 --release-artifact-base=registry.example/wamn/releases\n\
                 --release-manifest-digest=sha256:{seven}\n",
                seven = "7".repeat(64)
            )
        );
        assert_eq!(printed.lines().count(), 3);
    }

    #[test]
    fn the_release_coordinate_and_its_repository_are_all_required() {
        let complete =
            PrintProbe::try_parse_from(std::iter::once("print-release-env").chain(COORDINATE))
                .expect("the complete coordinate parses")
                .args;
        assert_eq!(complete.release_digest, RELEASE);
        assert_eq!(complete.artifact_base, "registry.example/wamn/releases");

        for omitted in [
            "--database-url",
            "--tenant",
            "--release-digest",
            "--artifact-base",
        ] {
            let mut argv = vec!["print-release-env"];
            let mut skip = false;
            for entry in COORDINATE {
                if entry == omitted {
                    skip = true;
                    continue;
                }
                if skip {
                    skip = false;
                    continue;
                }
                argv.push(entry);
            }
            assert!(
                PrintProbe::try_parse_from(argv).is_err(),
                "accepted a coordinate without {omitted}"
            );
        }
    }
}
