//! Arguments and output of the operational verbs of the `wamn-ctl-ops` binary.
//!
//! The work runs in `wamn-environment-control`. This module holds the clap surface and the
//! printed lines.

use clap::Args;
use wamn_broker::advisories::{EventAdvisoriesRequest, read_retained_advisories};
use wamn_environment_control::create_user::{CreateUserRequest, create_user_principal};
use wamn_environment_control::prune_record_history::{
    PruneRecordHistoryRequest, PrunedHistory, prune_expired_record_history,
};
use wamn_environment_control::prune_run_history::{
    PruneRunHistoryRequest, prune_terminal_run_history,
};

/// Retained broker advisory reporting arguments.
#[derive(Debug, Args)]
pub struct EventAdvisoriesArgs {
    /// Exact source stream from the broker advisory.
    #[arg(long)]
    pub stream: String,

    /// Exact durable consumer from the broker advisory.
    #[arg(long)]
    pub consumer: String,

    /// Maximum advisory records to print.
    #[arg(long, default_value_t = 100)]
    pub limit: usize,
}

/// Read the retained broker advisories of one consumer and print one JSON
/// record per line. The NATS connection comes from `WAMN_NATS_*`.
pub async fn event_advisories(args: EventAdvisoriesArgs) -> anyhow::Result<()> {
    let records = read_retained_advisories(EventAdvisoriesRequest {
        nats: wamn_broker::nats::PlatformNats::from_env()?,
        stream: args.stream,
        consumer: args.consumer,
        limit: args.limit,
    })
    .await?;
    for record in &records {
        println!("{}", serde_json::to_string(record)?);
    }
    Ok(())
}

/// Inputs that name one new user principal.
#[derive(Debug, Args)]
pub struct CreateUserArgs {
    /// Subject that the identity provider asserts for this user.
    #[arg(long)]
    pub subject: String,

    /// Deliverable address of this user. One address names one principal for
    /// the whole platform, so a user who works in two organizations holds one
    /// principal and two memberships.
    #[arg(long)]
    pub email: String,

    /// Name shown for this user.
    #[arg(long)]
    pub display_name: String,

    /// Provisioning administrator URL for the system database.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL", hide_env_values = true)]
    pub system_database_url: String,
}

/// Create one user principal and print its minted id.
///
/// The principal carries no access. Run `grant-project-env-membership` with the
/// printed id to admit the user to one project environment.
pub async fn create_user(args: CreateUserArgs) -> anyhow::Result<()> {
    let principal = create_user_principal(
        CreateUserRequest {
            subject: args.subject,
            email: args.email,
            display_name: args.display_name,
            system_database_url: args.system_database_url,
        },
        &wamn_broker::Publisher::connect(&wamn_broker::nats::PlatformNats::from_env()?).await?,
    )
    .await?;
    println!(
        "user created principal_id={} subject={}",
        principal.id(),
        principal.subject()
    );
    Ok(())
}

/// Record history retention arguments.
#[derive(Debug, Args)]
pub struct PruneRecordHistoryArgs {
    /// Postgres URL for this tenant's `wamn_audit_retention` credential
    /// generation. The verb refuses any other login. Env `WAMN_PG_URL`.
    #[arg(long, env = "WAMN_PG_URL", hide_env_values = true)]
    pub database_url: String,

    /// Organization in the coordinate that the mounted audit retention credential was minted for.
    /// A different coordinate refuses before any statement runs.
    #[arg(long)]
    pub org: String,
    /// Project in the coordinate that the mounted audit retention credential was minted for.
    /// A different coordinate refuses before any statement runs.
    #[arg(long)]
    pub project: String,
    /// Environment in the coordinate that the mounted audit retention credential was minted for.
    /// A different coordinate refuses before any statement runs.
    #[arg(long)]
    pub environment: String,
}

/// Terminal run history retention arguments.
#[derive(Debug, Args)]
pub struct PruneRunHistoryArgs {
    /// Postgres URL for this tenant's `wamn_run_retention` credential generation
    /// — the NOSUPERUSER/NOBYPASSRLS role whose whole authority is the terminal
    /// run delete. The verb refuses any other identity. Env `WAMN_PG_URL`.
    #[arg(long, env = "WAMN_PG_URL", hide_env_values = true)]
    pub database_url: String,

    /// The run-plane schema the `runs` table lives in (set as the session
    /// `search_path`). Bare identifier, and REQUIRED: the statement this
    /// verb drives is `DELETE FROM runs` — UNQUALIFIED — so it resolves through
    /// that session `search_path`. A default would let an invocation that omits
    /// the flag prune a relation the operator never named and still report
    /// success.
    #[arg(long)]
    pub schema: String,

    /// Organization in the coordinate that the mounted run retention credential was minted for.
    /// A different coordinate refuses before any statement runs.
    #[arg(long)]
    pub org: String,
    /// Project in the coordinate that the mounted run retention credential was minted for.
    /// A different coordinate refuses before any statement runs.
    #[arg(long)]
    pub project: String,
    /// Environment in the coordinate that the mounted run retention credential was minted for.
    /// A different coordinate refuses before any statement runs.
    #[arg(long)]
    pub environment: String,

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
        org: args.org.clone(),
        project: args.project.clone(),
        environment: args.environment.clone(),
    })
    .await?;
    print_pruned_record_history(
        &pruned,
        &wamn_control_registry::project_env_key(&args.org, &args.project, &args.environment),
    );
    Ok(())
}

fn print_pruned_record_history(pruned: &[PrunedHistory], env_key: &str) {
    for history in pruned {
        println!(
            "prune-record-history: removed {} entries from {}.{} (retention P{}D, tenant {})",
            history.removed, history.schema, history.history, history.days, env_key
        );
    }
    println!(
        "prune-record-history: pruned {} history table(s) for env_key {}",
        pruned.len(),
        env_key
    );
}

/// Prune one tenant's expired terminal run history and print the count.
pub async fn prune_run_history(args: PruneRunHistoryArgs) -> anyhow::Result<()> {
    let pruned = prune_terminal_run_history(PruneRunHistoryRequest {
        database_url: args.database_url,
        schema: args.schema.clone(),
        org: args.org.clone(),
        project: args.project.clone(),
        environment: args.environment.clone(),
        retention_days: args.retention_days,
        dry_run: args.dry_run,
    })
    .await?;

    if args.dry_run {
        println!(
            "prune-run-history (dry-run): {pruned} terminal run(s) older than {} day(s) WOULD be \
             pruned in schema {} (env_key {})",
            args.retention_days,
            args.schema,
            wamn_control_registry::project_env_key(&args.org, &args.project, &args.environment)
        );
    } else {
        println!(
            "prune-run-history: pruned {pruned} terminal run(s) older than {} day(s) in schema {} \
             (env_key {})",
            args.retention_days,
            args.schema,
            wamn_control_registry::project_env_key(&args.org, &args.project, &args.environment)
        );
    }
    Ok(())
}
