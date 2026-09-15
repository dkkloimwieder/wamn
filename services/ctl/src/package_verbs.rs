//! Arguments and output of the `apply-package`, `reconcile-package-data-access`, and
//! `reconcile-replica-identity` verbs.

use std::path::PathBuf;

use clap::Args;
use wamn_control::apply_package::{self, ApplyOutcome, ApplyPackageRequest};
use wamn_control::reconcile_package_data_access::{self, ReconcilePackageDataAccessRequest};
use wamn_control::reconcile_replica_identity::{
    ReconcileReplicaIdentityRequest, reconcile_package_replica_identity,
};
use wamn_schema_control::{ReplicaIdentity, ReplicaIdentityPlan};

/// Apply the immutable pending suffix from one package directory.
#[derive(Debug, Args)]
pub struct ApplyPackageArgs {
    /// Package root containing strict wamn.json and migrations/.
    #[arg(long)]
    pub package: PathBuf,

    /// Owner connection to the target project-environment database.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub database_url: String,

    /// Tenant stored with the package and migration records.
    #[arg(long)]
    pub tenant: String,
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
    let outcome = apply_package::apply_package(ApplyPackageRequest {
        package: args.package,
        database_url: args.database_url,
        tenant: args.tenant,
    })
    .await?;
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
