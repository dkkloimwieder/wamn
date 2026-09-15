//! Arguments and output of the `apply-package` and `reconcile-package-data-access` verbs.

use std::path::PathBuf;

use clap::Args;
use wamn_control::apply_package::{self, ApplyOutcome, ApplyPackageRequest};
use wamn_control::reconcile_package_data_access::{self, ReconcilePackageDataAccessRequest};

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
