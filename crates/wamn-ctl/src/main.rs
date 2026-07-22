//! wamn-ctl: the one-shot control-plane verbs binary (SR9).
//!
//! The nine verbs keep the exact subcommand names and flags they had under
//! the pre-split `wamn-host` binary — Job manifests change only which binary
//! runs (`command:`/image swap). The washlet / dispatcher / run-worker /
//! cdc-reader programs live in their own crates.

use std::str::FromStr as _;

use clap::{Parser, Subcommand};
use wamn_ctl::{
    copy_project_env, dump_project_env, enable_cdc_project_env, migrate_catalog, pin_run,
    provision, provision_org, provision_project_env, prune_run_history, publish_catalog,
    reconcile_replica_identity, reconcile_run_plane, restore_project_env,
};
// [11.8] wamn-wvb: appended so cherry-picks compose (sibling lanes touch this use block too).
use wamn_ctl::impact_report;

#[derive(Parser)]
#[command(name = "wamn-ctl", version, about)]
struct Cli {
    /// Log level (the chart passes this before the subcommand)
    #[arg(long = "log-level", global = true, default_value = "info")]
    log_level: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Write a project's catalog snapshot into the wamn_catalog table (4.1b)
    PublishCatalog(publish_catalog::PublishCatalogArgs),
    /// Provision a per-project Postgres database + credential on the shared cluster (2.3)
    ProvisionProject(provision::ProvisionProjectArgs),
    /// Render a dedicated org's CNPG Cluster set (one per recovery domain, sized by env policy) + record it in the T1 registry (wamn-q3n.6 / D18)
    ProvisionOrg(provision_org::ProvisionOrgArgs),
    /// Render a per-project-env database (CNPG Database CRD) + privilege step + record it in the T1 registry (wamn-q3n.7)
    ProvisionProjectEnv(provision_project_env::ProvisionProjectEnvArgs),
    /// Overlay CDC capture onto a provisioned project-env: publication + failover slot + replication role/Secret + reader registration (wamn-l5i9.9, D19 v3)
    EnableCdcProjectEnv(enable_cdc_project_env::EnableCdcProjectEnvArgs),
    /// Render/run per-project-env logical dumps (pg_dump -Fd → object storage; CronJob + on-demand) (wamn-q3n.10)
    DumpProjectEnv(dump_project_env::DumpProjectEnvArgs),
    /// Restore a per-project-env logical dump (pg_restore -Fd → scratch DB or in-place) (wamn-q3n.11)
    RestoreProjectEnv(restore_project_env::RestoreProjectEnvArgs),
    /// Copy a project-env to another (deploy/promote/clone/move): definition|data|both, quiesce-gated cutover (wamn-8df.5)
    CopyProjectEnv(copy_project_env::CopyProjectEnvArgs),
    /// Apply a catalog to a project DB: versioned, forward-only migration + lifecycle + history (2.5)
    MigrateCatalog(migrate_catalog::MigrateCatalogArgs),
    /// Reconcile per-entity REPLICA IDENTITY FULL from the catalog's registrations (old-image/delete needs) — idempotent ALTERs (wamn-l5i9.31)
    ReconcileReplicaIdentity(reconcile_replica_identity::ReconcileReplicaIdentityArgs),
    /// Reconcile a project-env's run-plane schema to deploy/sql — create missing tables, additive ALTERs, outbox-era teardown; idempotent (wamn-1wdq)
    ReconcileRunPlane(reconcile_run_plane::ReconcileRunPlaneArgs),
    /// Prune a project-env's TERMINAL run history older than --retention-days (9.6): app-role, tenant-scoped DELETE; node_runs cascade, cron_anchor untouched (wamn-srb)
    PruneRunHistory(prune_run_history::PruneRunHistoryArgs),
    /// Pin a recorded run as a versioned test case (11.3): secret-scrubbed + volatile-field-normalized, written to test_suites/test_cases; refuses an off/preview (non-replayable) run (wamn-htn)
    PinRun(pin_run::PinRunArgs),
    /// Report the schema-change impact of a --target catalog: affected entities (additive/destructive) → flows via event registration + node config → their suites → generated-API resources. Read-only, mutates nothing (11.8, wamn-wvb)
    ImpactReport(impact_report::ImpactReportArgs),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let level = tracing::Level::from_str(&cli.log_level)
        .map_err(|_| anyhow::anyhow!("invalid log level: {}", cli.log_level))?;
    // Same shape as the pre-split no-OTEL path of
    // wash_runtime::observability::initialize_observability: stderr fmt layer,
    // RUST_LOG overriding --log-level. The verbs report via stdout; this
    // carries dep diagnostics only.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level.as_str()));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    match cli.command {
        Command::PublishCatalog(args) => publish_catalog::run(args).await,
        Command::ProvisionProject(args) => provision::run(args).await,
        Command::ProvisionOrg(args) => provision_org::run(args).await,
        Command::ProvisionProjectEnv(args) => provision_project_env::run(args).await,
        Command::EnableCdcProjectEnv(args) => enable_cdc_project_env::run(args).await,
        Command::DumpProjectEnv(args) => dump_project_env::run(args).await,
        Command::RestoreProjectEnv(args) => restore_project_env::run(args).await,
        Command::CopyProjectEnv(args) => copy_project_env::run(args).await,
        Command::MigrateCatalog(args) => migrate_catalog::run(args).await,
        Command::ReconcileReplicaIdentity(args) => reconcile_replica_identity::run(args).await,
        Command::ReconcileRunPlane(args) => reconcile_run_plane::run(args).await,
        Command::PruneRunHistory(args) => prune_run_history::run(args).await,
        Command::PinRun(args) => pin_run::run(args).await,
        Command::ImpactReport(args) => impact_report::run(args).await,
    }
}
