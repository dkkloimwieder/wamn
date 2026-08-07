//! Deterministic product scenario worker.

use std::str::FromStr as _;

use clap::Parser;

#[derive(Parser)]
// `subcommand_negates_reqs` keeps the historical bare invocation — the one the
// suiteexec gate Job uses — exactly as it was, while letting `serve` skip the
// stored-suite arguments it has no use for.
#[command(
    name = "wamn-scenario-worker",
    version,
    about,
    subcommand_negates_reqs = true
)]
struct Cli {
    /// Log level.
    #[arg(long = "log-level", global = true, default_value = "info")]
    log_level: String,

    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    args: wamn_scenario_worker::ScenarioWorkerArgs,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Serve the authenticated management authoring surface.
    Serve(Box<wamn_scenario_worker::management::ManagementServeArgs>),
}

fn main() -> anyhow::Result<()> {
    wamn_runtime::advertise_memory_ceiling();
    async_main()
}

#[tokio::main]
async fn async_main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let level = tracing::Level::from_str(&cli.log_level)
        .map_err(|_| anyhow::anyhow!("invalid log level: {}", cli.log_level))?;
    let shutdown_observability =
        wash_runtime::observability::initialize_observability(level, false, false)?;

    let result = match cli.command {
        Some(Command::Serve(args)) => wamn_scenario_worker::management::serve(*args).await,
        None => wamn_scenario_worker::run(cli.args).await,
    };

    shutdown_observability();
    result
}
