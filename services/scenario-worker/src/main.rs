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
    // The compiled default, explicitly: this process has no pooling flags to
    // parse because it builds no engine and creates no store (wamn-t883).
    wamn_runtime::advertise_memory_ceiling(
        wamn_runtime::engine::PoolSizing::default().memory_cap_bytes,
    );
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

    /// wamn-0h0g.8.18: the authoring input is the control-database one, and the
    /// environment is fixed configuration rather than a per-request choice.
    /// wamn-0h0g.8.5.3 adds the second, separate project-database admission
    /// input alongside it.
    const SERVE: [&str; 16] = [
        "wamn-scenario-worker",
        "serve",
        "--system-url",
        "postgres://system.invalid/system",
        "--control-authoring-database-url",
        "postgres://control.invalid/wamn-system",
        "--management-admission-database-url",
        "postgres://project.invalid/wamn-db-acme--receiving--dev--k3m9x2p7",
        "--org",
        "acme",
        "--project",
        "receiving",
        "--environment",
        "dev",
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
        assert_eq!(args.environment, "dev");
        assert_eq!(args.tenant, "tenant-a");
        assert_eq!(args.system_url, "postgres://system.invalid/system");
        assert_eq!(
            args.control_authoring_database_url,
            "postgres://control.invalid/wamn-system"
        );
        assert_eq!(
            args.management_admission_database_url,
            "postgres://project.invalid/wamn-db-acme--receiving--dev--k3m9x2p7"
        );
        assert!(Cli::try_parse_from(["wamn-scenario-worker"]).is_err());
        assert!(Cli::try_parse_from(["wamn-scenario-worker", "--tenant", "tenant-a"]).is_err());
        // Every scope component is required: none of them has a default that
        // would silently bind this process to a scope nobody chose.
        for omitted in [
            "--control-authoring-database-url",
            "--management-admission-database-url",
            "--org",
            "--project",
            "--environment",
            "--tenant",
        ] {
            let partial: Vec<&str> = SERVE
                .iter()
                .copied()
                .enumerate()
                .filter(|(index, argument)| {
                    *argument != omitted && SERVE.get(index.wrapping_sub(1)) != Some(&omitted)
                })
                .map(|(_, argument)| argument)
                .collect();
            assert!(
                Cli::try_parse_from(partial).is_err(),
                "serve parsed without {omitted}"
            );
        }
    }

    /// wamn-0h0g.8.5.3: the admission input refuses at PARSE time, and the
    /// refusal NAMES the environment variable.
    ///
    /// The Secret this variable is fed from is minted by wamn-0h0g.12.176. Until
    /// that lands, an environment brought up without it crash-loops here, and
    /// the only thing an operator has to go on is this message — clap's default
    /// value name is the uppercased argument, which names no variable at all, so
    /// the explicit `value_name` is what makes the log actionable.
    #[test]
    fn the_admission_input_refusal_names_its_environment_variable() {
        let without: Vec<&str> = SERVE
            .iter()
            .copied()
            .enumerate()
            .filter(|(index, argument)| {
                *argument != "--management-admission-database-url"
                    && SERVE.get(index.wrapping_sub(1))
                        != Some(&"--management-admission-database-url")
            })
            .map(|(_, argument)| argument)
            .collect();
        let Err(error) = Cli::try_parse_from(without) else {
            panic!("serve parsed without the admission connection input");
        };
        let rendered = error.to_string();
        assert!(
            rendered.contains("WAMN_MANAGEMENT_ADMISSION_PG_URL"),
            "the missing-argument refusal does not name the variable: {rendered}"
        );
    }
}
