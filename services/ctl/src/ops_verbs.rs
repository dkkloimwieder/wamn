//! Arguments and output of the operational verbs of the `wamn-ctl-ops` binary.
//!
//! The work runs in `wamn-control`. This module holds the clap surface and the
//! printed lines.

use clap::Args;
use wamn_control::prune_record_history::{
    PruneRecordHistoryRequest, PrunedHistory, prune_expired_record_history,
};
use wamn_control::prune_run_history::{PruneRunHistoryRequest, prune_terminal_run_history};

/// Record history retention arguments.
#[derive(Debug, Args)]
pub struct PruneRecordHistoryArgs {
    /// Postgres URL for this tenant's `wamn_audit_retention` credential
    /// generation. The verb refuses any other login. Env `WAMN_PG_URL`.
    #[arg(long, env = "WAMN_PG_URL")]
    pub database_url: String,

    /// The tenant that the mounted audit retention credential was minted for.
    /// A different tenant refuses before any statement runs.
    #[arg(long)]
    pub tenant: String,
}

/// Terminal run history retention arguments.
#[derive(Debug, Args)]
pub struct PruneRunHistoryArgs {
    /// Postgres URL for this tenant's `wamn_run_retention` credential generation
    /// — the NOSUPERUSER/NOBYPASSRLS role whose whole authority is the terminal
    /// run delete. The verb refuses any other identity. Env `WAMN_PG_URL`.
    #[arg(long, env = "WAMN_PG_URL")]
    pub database_url: String,

    /// The run-plane schema the `runs` table lives in (set as the session
    /// `search_path`). Bare identifier, and REQUIRED: the statement this
    /// verb drives is `DELETE FROM runs` — UNQUALIFIED — so it resolves through
    /// that session `search_path`. A default would let an invocation that omits
    /// the flag prune a relation the operator never named and still report
    /// success.
    #[arg(long)]
    pub schema: String,

    /// The tenant whose run history to prune. It must be the tenant the mounted
    /// retention credential was minted for; a mismatch refuses loudly rather
    /// than pruning nothing and reporting success.
    #[arg(long)]
    pub tenant: String,

    /// Prune terminal runs whose `created_at` is older than this many days.
    #[arg(long)]
    pub retention_days: u32,

    /// Count what WOULD be pruned (a rolled-back delete under the same predicate)
    /// without deleting anything.
    #[arg(long)]
    pub dry_run: bool,
}

/// Prune the expired record history of one tenant and print what each history
/// table lost.
pub async fn prune_record_history(args: PruneRecordHistoryArgs) -> anyhow::Result<()> {
    let pruned = prune_expired_record_history(PruneRecordHistoryRequest {
        database_url: args.database_url,
        tenant: args.tenant.clone(),
    })
    .await?;
    print_pruned_record_history(&pruned, &args.tenant);
    Ok(())
}

fn print_pruned_record_history(pruned: &[PrunedHistory], tenant: &str) {
    for history in pruned {
        println!(
            "prune-record-history: removed {} entries from {}.{} (retention P{}D, tenant {})",
            history.removed, history.schema, history.history, history.days, tenant
        );
    }
    println!(
        "prune-record-history: pruned {} history table(s) for tenant {}",
        pruned.len(),
        tenant
    );
}

/// Prune one tenant's expired terminal run history and print the count.
pub async fn prune_run_history(args: PruneRunHistoryArgs) -> anyhow::Result<()> {
    let pruned = prune_terminal_run_history(PruneRunHistoryRequest {
        database_url: args.database_url,
        schema: args.schema.clone(),
        tenant: args.tenant.clone(),
        retention_days: args.retention_days,
        dry_run: args.dry_run,
    })
    .await?;

    if args.dry_run {
        println!(
            "prune-run-history (dry-run): {pruned} terminal run(s) older than {} day(s) WOULD be \
             pruned in schema {} (tenant {})",
            args.retention_days, args.schema, args.tenant
        );
    } else {
        println!(
            "prune-run-history: pruned {pruned} terminal run(s) older than {} day(s) in schema {} \
             (tenant {})",
            args.retention_days, args.schema, args.tenant
        );
    }
    Ok(())
}
