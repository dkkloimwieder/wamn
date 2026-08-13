//! Operational one-shot control-plane verbs.

use std::str::FromStr as _;

use clap::{Parser, Subcommand};
use wamn_ctl::{
    copy_project_env, dump_project_env, impact_report, prune_run_history, restore_project_env,
};

#[derive(Parser)]
#[command(name = "wamn-ctl-ops", version, about)]
struct Cli {
    /// Log level.
    #[arg(long = "log-level", global = true, default_value = "info")]
    log_level: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Render or run per-project-env logical dumps.
    DumpProjectEnv(dump_project_env::DumpProjectEnvArgs),
    /// Restore a per-project-env logical dump.
    RestoreProjectEnv(restore_project_env::RestoreProjectEnvArgs),
    /// Copy a project-env to another environment.
    CopyProjectEnv(copy_project_env::CopyProjectEnvArgs),
    /// Prune terminal run history older than the retention period.
    PruneRunHistory(prune_run_history::PruneRunHistoryArgs),
    /// Report the schema-change impact of a target catalog.
    ImpactReport(impact_report::ImpactReportArgs),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let level = tracing::Level::from_str(&cli.log_level)
        .map_err(|_| anyhow::anyhow!("invalid log level: {}", cli.log_level))?;
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level.as_str()));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    match cli.command {
        Command::DumpProjectEnv(args) => dump_project_env::run(args).await,
        Command::RestoreProjectEnv(args) => restore_project_env::run(args).await,
        Command::CopyProjectEnv(args) => copy_project_env::run(args).await,
        Command::PruneRunHistory(args) => prune_run_history::run(args).await,
        Command::ImpactReport(args) => impact_report::run(args).await,
    }
}
