//! Arguments and output of the `apply-package`, `push-package`,
//! and `reconcile-package-data-access` verbs.

use std::path::PathBuf;

use anyhow::Context as _;
use clap::Args;
use wamn_environment_control::reconcile_package_data_access::{
    self, ReconcilePackageDataAccessRequest,
};
use wamn_package_control::apply_package::{self, ApplyOutcome, ApplyPackageRequest, PackageRoot};
use wamn_package_control::package_artifact::{
    self, OpenedPackage, PackagePushDisposition, PackageRegistry, PackageSource, PushPackageRequest,
};
use wamn_runtime::component_artifact_source::OCI_CA_PATHS_ENV;

/// Apply the immutable pending suffix from one package artifact.
///
/// No verb applies a checkout directory, so the installed artifact digest is
/// the digest the registry records (docs/operations/building.md#reading-a-pushed-package).
#[derive(Debug, Args)]
pub struct ApplyPackageArgs {
    /// Repository of the component binaries that package descriptors name.
    #[arg(long, env = "WAMN_COMPONENT_ARTIFACT_BASE", required = true)]
    pub component_artifact_base: Option<String>,

    /// Package artifact `<package_id>-<version>` that `push-package` pushed.
    #[arg(long)]
    pub package_artifact: String,

    /// The registry flags of `push-package`.
    #[command(flatten)]
    pub registry: RegistryArgs,

    /// Owner connection to the target project-environment database.
    #[arg(long, env = "WAMN_PG_ADMIN_URL", hide_env_values = true)]
    pub database_url: String,

    #[arg(long)]
    pub org: String,
    /// Project whose environment owns the package and migration records.
    #[arg(long)]
    pub project: String,
    #[arg(long)]
    pub environment: String,
}

/// The registry flags of `apply-package` and
/// `reconcile-package-data-access`.
#[derive(Debug, Args)]
pub struct RegistryArgs {
    /// Explicit `<registry>/<repository>` base for package artifacts.
    #[arg(long)]
    pub artifact_base: String,

    /// `.dockerconfigjson` file carrying the registry credential.
    #[arg(long)]
    pub registry_auth_file: PathBuf,

    /// Use plain HTTP for exactly the registry in `--artifact-base`.
    #[arg(long, default_value_t = false)]
    pub insecure_registry: bool,

    /// PEM CA bundle trusted for the registry, on top of the compiled-in
    /// roots. Repeat or comma-delimit. Env `WASH_OCI_CA_PATHS`.
    #[arg(long = "oci-ca-path", env = OCI_CA_PATHS_ENV, value_delimiter = ',')]
    pub oci_ca_paths: Vec<PathBuf>,
}

impl RegistryArgs {
    pub(crate) fn registry(&self) -> PackageRegistry {
        PackageRegistry {
            artifact_base: self.artifact_base.clone(),
            credentials: None,
            registry_auth_file: self.registry_auth_file.clone(),
            insecure_registry: self.insecure_registry,
            oci_ca_paths: self.oci_ca_paths.clone(),
        }
    }
}

/// Open the package artifact `tag`.
async fn open(registry: &PackageRegistry, tag: String) -> anyhow::Result<OpenedPackage> {
    package_artifact::open_package_source(PackageSource::Artifact {
        tag,
        registry: registry.clone(),
    })
    .await
}

/// Open each package artifact of `tags`.
async fn open_all(
    registry: &PackageRegistry,
    tags: Vec<String>,
) -> anyhow::Result<Vec<OpenedPackage>> {
    let mut opened = Vec::with_capacity(tags.len());
    for tag in tags {
        opened.push(open(registry, tag).await?);
    }
    Ok(opened)
}

fn roots(opened: &[OpenedPackage]) -> Vec<PackageRoot> {
    opened.iter().map(OpenedPackage::package_root).collect()
}

/// Push one authored package as a registry artifact.
#[derive(Debug, Args)]
pub struct PushPackageArgs {
    /// Root of the authored package (it holds `wamn.k`). Its built components
    /// are read through `outputs.components` of its `build.json`, which must
    /// be current for the tree.
    #[arg(long)]
    pub package: PathBuf,

    /// Explicit `<registry>/<repository>` base for package artifacts.
    #[arg(long)]
    pub artifact_base: String,

    /// Explicit `<registry>/<repository>` base for component artifacts. Each
    /// component is pushed under its digest there before the package artifact.
    #[arg(long)]
    pub component_artifact_base: String,

    /// `.dockerconfigjson` file carrying the push credential.
    #[arg(long, env = "WAMN_REGISTRY_AUTH_FILE")]
    pub registry_auth_file: PathBuf,

    /// Use plain HTTP for exactly the registry in `--artifact-base`.
    #[arg(long, default_value_t = false)]
    pub insecure_registry: bool,

    /// PEM CA bundle trusted for the registry, on top of the compiled-in
    /// roots. Repeat or comma-delimit. Env `WASH_OCI_CA_PATHS`.
    #[arg(long = "oci-ca-path", env = OCI_CA_PATHS_ENV, value_delimiter = ',')]
    pub oci_ca_paths: Vec<PathBuf>,
}

/// Push one package and print its tag and digest.
pub async fn push(args: PushPackageArgs) -> anyhow::Result<()> {
    let pushed = package_artifact::push_package(&PushPackageRequest {
        package: args.package,
        registry: PackageRegistry {
            artifact_base: args.artifact_base,
            credentials: None,
            registry_auth_file: args.registry_auth_file,
            insecure_registry: args.insecure_registry,
            oci_ca_paths: args.oci_ca_paths,
        },
        component_artifact_base: args.component_artifact_base,
    })
    .await?;
    match pushed.disposition {
        PackagePushDisposition::Pushed => println!("pushed {} {}", pushed.tag, pushed.digest),
        PackagePushDisposition::AlreadyPresent => {
            println!("already present {} {}", pushed.tag, pushed.digest);
        }
    }
    Ok(())
}

/// Post-apply generated ACL reconciliation arguments.
#[derive(Debug, Args)]
pub struct ReconcilePackageDataAccessArgs {
    /// Installed package artifacts `<package_id>-<version>`, one for each
    /// applied package.
    #[arg(long = "package-artifact", required = true)]
    pub packages: Vec<String>,

    /// The registry flags of `push-package`.
    #[command(flatten)]
    pub registry: RegistryArgs,

    /// Owner connection to the target project-environment database.
    #[arg(long, env = "WAMN_PG_ADMIN_URL", hide_env_values = true)]
    pub database_url: String,

    #[arg(long)]
    pub org: String,
    /// Project whose environment owns the already-applied package coordinate.
    #[arg(long)]
    pub project: String,
    #[arg(long)]
    pub environment: String,
}

/// Apply one package artifact and print the applied line.
pub async fn apply(args: ApplyPackageArgs) -> anyhow::Result<()> {
    let registry = args.registry.registry();
    let opened = open(&registry, args.package_artifact).await?;
    let base = args
        .component_artifact_base
        .as_deref()
        .context("package install requires --component-artifact-base")?;
    let component_source = Some(
        wamn_runtime::component_artifact_source::ComponentArtifactSourceConfig::new(
            base,
            registry.insecure_registry,
            std::time::Duration::from_secs(30),
        )?
        .with_registry_auth_file(&registry.registry_auth_file)?
        .with_ca_paths(&registry.oci_ca_paths)?,
    );
    let request = ApplyPackageRequest {
        component_source,
        changes: wamn_platform_identity::auth_changes::AuthChanges::connect(
            &wamn_platform_identity::nats::PlatformNats::from_env()?,
        )
        .await?,
        package: opened.package_root(),
        database_url: args.database_url,
        org: args.org.clone(),
        project: args.project.clone(),
        environment: args.environment.clone(),
    };
    let outcome = apply_package::apply_package(request).await?;
    print_applied(&outcome);
    Ok(())
}

/// Print the line that reports one package application.
pub fn print_applied(outcome: &ApplyOutcome) {
    println!(
        "applied {}@{}: {} migration(s){}",
        outcome.package_id,
        outcome.package_version,
        outcome.migrations_applied,
        if outcome.changed {
            ""
        } else {
            " (already converged)"
        }
    );
}

/// Reconcile the exact installed set of generated package contributions.
pub async fn reconcile_data_access(args: ReconcilePackageDataAccessArgs) -> anyhow::Result<()> {
    let opened = open_all(&args.registry.registry(), args.packages).await?;
    let outcome = reconcile_package_data_access::reconcile_package_data_access(
        ReconcilePackageDataAccessRequest {
            packages: roots(&opened),
            database_url: args.database_url,
            org: args.org.clone(),
            project: args.project.clone(),
            environment: args.environment.clone(),
        },
    )
    .await?;
    println!(
        "reconciled data access for [{}]{}",
        outcome.coordinates().join(", "),
        if outcome.is_noop() {
            " (already converged)"
        } else {
            ""
        }
    );
    Ok(())
}
