//! Deterministic product scenario worker.

use std::str::FromStr as _;

use clap::Parser;

#[derive(Parser)]
#[command(name = "wamn-scenario-worker", version, about)]
struct Cli {
    /// Log level.
    #[arg(long = "log-level", global = true, default_value = "info")]
    log_level: String,

    #[command(subcommand)]
    command: Command,
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
        Command::Serve(args) => wamn_scenario_worker::management::serve(*args).await,
    };

    shutdown_observability();
    result
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory as _, Parser as _};

    use super::*;

    const SERVE: [&str; 12] = [
        "wamn-scenario-worker",
        "serve",
        "--system-url",
        "postgres://system.invalid/system",
        "--authoring-database-url",
        "postgres://project.invalid/project",
        "--org",
        "acme",
        "--project",
        "receiving",
        "--tenant",
        "tenant-a",
    ];

    #[test]
    fn the_cli_definition_is_internally_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn serve_is_the_only_invocation() {
        let cli = Cli::try_parse_from(SERVE).expect("serve parses from its complete arguments");
        let Command::Serve(args) = cli.command;
        assert_eq!(args.org, "acme");
        assert_eq!(args.project, "receiving");
        assert_eq!(args.tenant, "tenant-a");
        assert_eq!(args.system_url, "postgres://system.invalid/system");

        assert!(Cli::try_parse_from(["wamn-scenario-worker"]).is_err());
        assert!(Cli::try_parse_from(["wamn-scenario-worker", "--tenant", "tenant-a"]).is_err());
    }
}
