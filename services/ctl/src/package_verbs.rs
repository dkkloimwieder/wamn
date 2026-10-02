//! Arguments and output of the `apply-package`, `push-package`,
//! `reconcile-package-data-access`, and `reconcile-replica-identity` verbs.

use std::path::PathBuf;

use anyhow::Context as _;
use clap::Args;
use wamn_control::apply_package::{self, ApplyOutcome, ApplyPackageRequest};
use wamn_control::package_artifact::{
    self, PackagePushDisposition, PackageRegistry, PackageSource, PushPackageRequest,
};
use wamn_control::qualify_upgrade::{self, QualifyUpgradeRequest, workload::WorkloadTarget};
use wamn_control::reconcile_package_data_access::{self, ReconcilePackageDataAccessRequest};
use wamn_control::reconcile_replica_identity::{
    ReconcileReplicaIdentityRequest, reconcile_package_replica_identity,
};
use wamn_runtime::component_artifact_source::OCI_CA_PATHS_ENV;
use wamn_schema_control::{ReplicaIdentity, ReplicaIdentityPlan};

/// Apply the immutable pending suffix from one package directory or artifact.
#[derive(Debug, Args)]
pub struct ApplyPackageArgs {
    /// Package root containing strict wamn.json and migrations/.
    #[arg(
        long,
        required_unless_present = "package_artifact",
        conflicts_with = "package_artifact"
    )]
    pub package: Option<PathBuf>,

    /// Package artifact `<package_id>-<version>` that `push-package` pushed.
    /// It is fetched only when its digest equals the one in
    /// `catalog.package_artifacts`.
    #[arg(long, requires_all = ["artifact_base", "registry_auth_file", "control_database_url"])]
    pub package_artifact: Option<String>,

    /// The registry flags of `push-package`, for `--package-artifact`.
    #[command(flatten)]
    pub registry: OptionalRegistryArgs,

    /// Owner connection to the target project-environment database.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub database_url: String,

    /// Tenant stored with the package and migration records.
    #[arg(long)]
    pub tenant: String,

    /// Exact qualification result for an installed successor migration.
    #[arg(long)]
    pub upgrade_qualification: Option<PathBuf>,
}

/// Prove one package successor against the installed predecessor snapshot.
#[derive(Debug, Args)]
pub struct QualifyUpgradeArgs {
    /// Candidate package root.
    #[arg(long)]
    pub package: PathBuf,
    /// Complete package roots for the resulting installed environment.
    #[arg(long = "presented-package", required = true)]
    pub presented_packages: Vec<PathBuf>,
    /// Owner connection to the installed predecessor database.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub database_url: String,
    /// Tenant that owns the package lineage.
    #[arg(long)]
    pub tenant: String,
    /// Environment whose selected release currently serves.
    #[arg(long)]
    pub environment: String,
    /// New file for canonical qualification evidence.
    #[arg(long)]
    pub result: PathBuf,
    /// Kubernetes configuration for the serving environment.
    #[arg(long)]
    pub kubeconfig: PathBuf,
    /// Explicit Kubernetes context.
    #[arg(long)]
    pub context: String,
    /// Namespace that contains the serving host and application workloads.
    #[arg(long)]
    pub namespace: String,
    /// Host Deployment that serves the selected release.
    #[arg(long)]
    pub host_deployment: String,
    /// SQL-bearing package and its serving WorkloadDeployment, PACKAGE=NAME.
    #[arg(long = "package-workload", value_parser = parse_package_workload)]
    pub package_workloads: Vec<(String, String)>,
}

fn parse_package_workload(value: &str) -> Result<(String, String), String> {
    let (package, workload) = value
        .split_once('=')
        .ok_or_else(|| "use PACKAGE=WORKLOAD_DEPLOYMENT".to_owned())?;
    if package.is_empty() || workload.is_empty() || workload.contains('=') {
        return Err("use a nonempty PACKAGE=WORKLOAD_DEPLOYMENT".to_owned());
    }
    Ok((package.to_owned(), workload.to_owned()))
}

/// Qualify the database transition and print its immutable result identity.
pub async fn qualify_upgrade(args: QualifyUpgradeArgs) -> anyhow::Result<()> {
    let mut package_workloads = std::collections::BTreeMap::new();
    for (package, workload) in args.package_workloads {
        anyhow::ensure!(
            package_workloads.insert(package, workload).is_none(),
            "a package has more than one workload selector"
        );
    }
    let outcome = qualify_upgrade::qualify_upgrade(QualifyUpgradeRequest {
        database_url: args.database_url,
        tenant: args.tenant,
        environment: args.environment,
        package: args.package,
        presented_packages: args.presented_packages,
        result: args.result,
        workload: WorkloadTarget {
            kubeconfig: args.kubeconfig,
            context: args.context,
            namespace: args.namespace,
            host_deployment: args.host_deployment,
            package_workloads,
        },
    })
    .await?;
    println!(
        "qualified upgrade {}@{}: {} {}",
        outcome.package_id,
        outcome.package_version,
        outcome.sha256,
        outcome.result.display()
    );
    Ok(())
}

/// The registry flags of `apply-package --package-artifact`.
#[derive(Debug, Args)]
pub struct OptionalRegistryArgs {
    /// Explicit `<registry>/<repository>` base for package artifacts.
    #[arg(long, requires = "package_artifact")]
    pub artifact_base: Option<String>,

    /// `.dockerconfigjson` file carrying the registry credential.
    #[arg(long, requires = "package_artifact")]
    pub registry_auth_file: Option<PathBuf>,

    /// Use plain HTTP for exactly the registry in `--artifact-base`.
    #[arg(long, default_value_t = false)]
    pub insecure_registry: bool,

    /// PEM CA bundle trusted for the registry, on top of the compiled-in
    /// roots. Repeat or comma-delimit. Env `WASH_OCI_CA_PATHS`.
    #[arg(long = "oci-ca-path", env = OCI_CA_PATHS_ENV, value_delimiter = ',')]
    pub oci_ca_paths: Vec<PathBuf>,

    /// Owner URL of the control database that holds `catalog.package_artifacts`.
    #[arg(long, requires = "package_artifact")]
    pub control_database_url: Option<String>,
}

/// Push one authored package as a registry artifact.
#[derive(Debug, Args)]
pub struct PushPackageArgs {
    /// Root of the authored package (it holds `wamn.k`). Its built components
    /// are read through `../target/components.json`, the build index that
    /// `tools/build-components` writes.
    #[arg(long)]
    pub package: PathBuf,

    /// Explicit `<registry>/<repository>` base for package artifacts.
    #[arg(long)]
    pub artifact_base: String,

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

    /// Owner URL of the control database that records the artifact in
    /// `catalog.package_artifacts`.
    #[arg(long)]
    pub control_database_url: String,

    /// Source commit recorded with the artifact.
    #[arg(long)]
    pub source_commit: Option<String>,
}

/// Push one package and print its tag and digest.
pub async fn push(args: PushPackageArgs) -> anyhow::Result<()> {
    let pushed = package_artifact::push_package(&PushPackageRequest {
        package: args.package,
        registry: PackageRegistry {
            artifact_base: args.artifact_base,
            registry_auth_file: args.registry_auth_file,
            insecure_registry: args.insecure_registry,
            oci_ca_paths: args.oci_ca_paths,
            control_database_url: args.control_database_url,
        },
        source_commit: args.source_commit,
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
    /// Installed package roots containing wamn.json and generated policy evidence.
    #[arg(long = "package", required = true)]
    pub packages: Vec<PathBuf>,

    /// Owner connection to the target project-environment database.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub database_url: String,

    /// Tenant owning the already-applied package coordinate.
    #[arg(long)]
    pub tenant: String,
}

/// Replica identity reconciliation arguments.
#[derive(Debug, Args)]
pub struct ReconcileReplicaIdentityArgs {
    /// Superuser connection to the project database.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,

    /// Package root whose strict manifest maps model keys to physical tables.
    #[arg(long)]
    pub package: PathBuf,

    /// Print the plan without applying it.
    #[arg(long)]
    pub dry_run: bool,
}

/// Apply one package directory and print the applied line.
pub async fn apply(args: ApplyPackageArgs) -> anyhow::Result<()> {
    let source = match (args.package, args.package_artifact) {
        (Some(package), None) => PackageSource::Directory(package),
        (None, Some(tag)) => {
            let registry = args.registry;
            PackageSource::Artifact {
                tag,
                registry: PackageRegistry {
                    artifact_base: registry
                        .artifact_base
                        .context("--artifact-base is required")?,
                    registry_auth_file: registry
                        .registry_auth_file
                        .context("--registry-auth-file is required")?,
                    insecure_registry: registry.insecure_registry,
                    oci_ca_paths: registry.oci_ca_paths,
                    control_database_url: registry
                        .control_database_url
                        .context("--control-database-url is required")?,
                },
            }
        }
        _ => anyhow::bail!("give one of --package or --package-artifact"),
    };
    let opened = package_artifact::open_package_source(source).await?;
    let request = ApplyPackageRequest {
        package: opened.root().to_path_buf(),
        database_url: args.database_url,
        tenant: args.tenant,
    };
    let outcome = if let Some(path) = args.upgrade_qualification {
        apply_package::apply_qualified_package(request, &path).await?
    } else {
        apply_package::apply_package(request).await?
    };
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
    let outcome = reconcile_package_data_access::reconcile_package_data_access(
        ReconcilePackageDataAccessRequest {
            packages: args.packages,
            database_url: args.database_url,
            tenant: args.tenant,
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

/// Reconcile the replica identity of one package's models and print the plan lines.
pub async fn reconcile_replica_identity(args: ReconcileReplicaIdentityArgs) -> anyhow::Result<()> {
    let plan = reconcile_package_replica_identity(ReconcileReplicaIdentityRequest {
        admin_database_url: args.admin_database_url,
        package: args.package,
        dry_run: args.dry_run,
    })
    .await?;
    print_replica_identity_plan(&plan, args.dry_run);
    Ok(())
}

fn identity_keyword(identity: ReplicaIdentity) -> &'static str {
    match identity {
        ReplicaIdentity::Full => "FULL",
        ReplicaIdentity::Default => "DEFAULT",
    }
}

fn print_replica_identity_plan(plan: &ReplicaIdentityPlan, dry_run: bool) {
    let verb = if dry_run { "would flip" } else { "flipped" };
    if plan.flips.is_empty() {
        println!(
            "replica identity already reconciled: {} model(s) at target",
            plan.unchanged.len()
        );
    }
    for flip in &plan.flips {
        println!(
            "{verb} {}.{} ({}): {} -> {}",
            flip.schema,
            flip.table,
            flip.model_id,
            identity_keyword(flip.from),
            identity_keyword(flip.to)
        );
    }
    for table in &plan.skipped_absent {
        println!("[skip] {table} is absent; apply the package before reconciling");
    }
}
