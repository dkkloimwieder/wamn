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
use wamn_host::{dispatch, host, provision, provision_org, publish_catalog};

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
    /// Write a project's catalog snapshot into the wamn_catalog table (4.1b)
    PublishCatalog(publish_catalog::PublishCatalogArgs),
    /// Provision a per-project Postgres database + credential on the shared cluster (2.3)
    ProvisionProject(provision::ProvisionProjectArgs),
    /// Render a paying org's CNPG Cluster PAIR (prod HA + dev hibernation) + record it in the T1 registry (wamn-q3n.6)
    ProvisionOrg(provision_org::ProvisionOrgArgs),
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
        Command::PublishCatalog(args) => publish_catalog::run(args).await,
        Command::ProvisionProject(args) => provision::run(args).await,
        Command::ProvisionOrg(args) => provision_org::run(args).await,
    };

    shutdown_observability();
    result
}
