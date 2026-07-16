//! wamn-host: the production host binary.
//!
//! `host`            — ClusterHost driven by the runtime-operator over NATS.
//! `dispatch`        — the shared trigger dispatcher (cron + outbox + wakes).
//! `publish-catalog` — project provisioning + catalog snapshot publication.
//!
//! The gate suite (bench/pgbench/…/f1proof) lives in the separate
//! `wamn-gates` binary (docs/structure-review.md SR1); this artifact ships
//! none of it.

use std::str::FromStr as _;

use clap::{Parser, Subcommand};
use wamn_host::{
    dispatch, dump_project_env, host, migrate_catalog, move_org_tier, provision, provision_org,
    provision_project_env, publish_catalog, restore_project_env, run_worker,
};

#[derive(Parser)]
#[command(name = "wamn-host", version, about)]
struct Cli {
    /// Log level (the chart passes this before the subcommand)
    #[arg(long = "log-level", global = true, default_value = "info")]
    log_level: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the wamn host (operator-managed via NATS)
    Host(Box<host::HostArgs>),
    /// Run the shared trigger dispatcher (5.14): cron + outbox + parked-wake across all projects
    Dispatch(dispatch::DispatchArgs),
    /// Run the production flow runner (5.14): claim from run_queue + drive the flowrunner, looping (fqg.8)
    RunWorker(run_worker::RunWorkerArgs),
    /// Write a project's catalog snapshot into the wamn_catalog table (4.1b)
    PublishCatalog(publish_catalog::PublishCatalogArgs),
    /// Provision a per-project Postgres database + credential on the shared cluster (2.3)
    ProvisionProject(provision::ProvisionProjectArgs),
    /// Render a paying org's CNPG Cluster PAIR (prod HA + dev hibernation) + record it in the T1 registry (wamn-q3n.6)
    ProvisionOrg(provision_org::ProvisionOrgArgs),
    /// Render a per-project-env database (CNPG Database CRD) + privilege step + record it in the T1 registry (wamn-q3n.7)
    ProvisionProjectEnv(provision_project_env::ProvisionProjectEnvArgs),
    /// Render/run per-project-env logical dumps (pg_dump -Fd → object storage; CronJob + on-demand) (wamn-q3n.10)
    DumpProjectEnv(dump_project_env::DumpProjectEnvArgs),
    /// Restore a per-project-env logical dump (pg_restore -Fd → scratch DB or in-place) (wamn-q3n.11)
    RestoreProjectEnv(restore_project_env::RestoreProjectEnvArgs),
    /// Promote an org to a higher tier: T3→T2 / T2→T4 (dump→provision→restore→flip) (wamn-q3n.13)
    MoveOrgTier(move_org_tier::MoveOrgTierArgs),
    /// Apply a catalog to a project DB: versioned, forward-only migration + lifecycle + history (2.5)
    MigrateCatalog(migrate_catalog::MigrateCatalogArgs),
}

fn main() -> anyhow::Result<()> {
    wamn_host::advertise_memory_ceiling();
    async_main()
}

#[tokio::main]
async fn async_main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let level = tracing::Level::from_str(&cli.log_level)
        .map_err(|_| anyhow::anyhow!("invalid log level: {}", cli.log_level))?;
    // OTel exporters activate when OTEL_* env vars are present.
    let shutdown_observability =
        wash_runtime::observability::initialize_observability(level, false, false)?;

    let result = match cli.command {
        Command::Host(args) => host::run(*args).await,
        Command::Dispatch(args) => dispatch::run(args).await,
        Command::RunWorker(args) => run_worker::run(args).await,
        Command::PublishCatalog(args) => publish_catalog::run(args).await,
        Command::ProvisionProject(args) => provision::run(args).await,
        Command::ProvisionOrg(args) => provision_org::run(args).await,
        Command::ProvisionProjectEnv(args) => provision_project_env::run(args).await,
        Command::DumpProjectEnv(args) => dump_project_env::run(args).await,
        Command::RestoreProjectEnv(args) => restore_project_env::run(args).await,
        Command::MoveOrgTier(args) => move_org_tier::run(args).await,
        Command::MigrateCatalog(args) => migrate_catalog::run(args).await,
    };

    shutdown_observability();
    result
}
