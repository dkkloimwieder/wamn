//! Operational one-shot control-plane verbs.

use std::str::FromStr as _;

use clap::{Parser, Subcommand};
use wamn_ctl::ops_verbs;

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
    DumpProjectEnv(ops_verbs::DumpProjectEnvArgs),
    /// Restore a per-project-env logical dump.
    RestoreProjectEnv(ops_verbs::RestoreProjectEnvArgs),
    /// Copy a project-env to another environment.
    CopyProjectEnv(ops_verbs::CopyProjectEnvArgs),
    /// Prune terminal run history older than the retention period.
    PruneRunHistory(ops_verbs::PruneRunHistoryArgs),
    /// Remove expired record history entries as the audit retention task.
    PruneRecordHistory(ops_verbs::PruneRecordHistoryArgs),
    /// Read retained broker advisories and the available source payloads.
    EventAdvisories(ops_verbs::EventAdvisoriesArgs),
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
        Command::DumpProjectEnv(args) => ops_verbs::dump_project_env(args).await,
        Command::RestoreProjectEnv(args) => ops_verbs::restore_project_env(args).await,
        Command::CopyProjectEnv(args) => ops_verbs::copy_project_env(args).await,
        Command::PruneRunHistory(args) => ops_verbs::prune_run_history(args).await,
        Command::PruneRecordHistory(args) => ops_verbs::prune_record_history(args).await,
        Command::EventAdvisories(args) => ops_verbs::event_advisories(args).await,
    }
}
