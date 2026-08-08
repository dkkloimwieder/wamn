//! Deterministic product scenario worker.

use std::str::FromStr as _;

use clap::Parser;

#[derive(Parser)]
// This binary has two invocations: the historical bare one the suiteexec gate
// Job uses, and `serve`. Keeping them apart takes both settings below plus the
// `Option` on the flattened group.
//
// `subcommand_negates_reqs` alone is NOT enough, which is the wamn-kisz defect:
// it relaxes the usage line but still enforces a REQUIRED flattened struct, so
// `serve` with every one of its own arguments satisfied still died on the bare
// path's `--tenant`. Making the flattened group `Option` is what actually
// relaxes it — clap then demands the whole stored-suite set as soon as any part
// of it appears, and demands none of it when none of it appears.
//
// `arg_required_else_help` preserves the one behaviour `Option` would otherwise
// lose: an invocation with neither a subcommand nor any argument must not
// silently become a bare run.
#[command(
    name = "wamn-scenario-worker",
    version,
    about,
    subcommand_negates_reqs = true,
    arg_required_else_help = true
)]
struct Cli {
    /// Log level.
    #[arg(long = "log-level", global = true, default_value = "info")]
    log_level: String,

    #[command(subcommand)]
    command: Option<Command>,

    /// Present exactly when no subcommand was given.
    #[command(flatten)]
    args: Option<wamn_scenario_worker::ScenarioWorkerArgs>,
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

    let result = match (cli.command, cli.args) {
        (Some(Command::Serve(args)), _) => wamn_scenario_worker::management::serve(*args).await,
        (None, Some(args)) => wamn_scenario_worker::run(args).await,
        // Unreachable through the CLI: `arg_required_else_help` answers the
        // empty invocation, and any part of the stored-suite set requires all
        // of it. Refusing beats inventing a run out of defaults.
        (None, None) => Err(anyhow::anyhow!(
            "no subcommand and no stored-suite arguments were given"
        )),
    };

    shutdown_observability();
    result
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory as _, Parser as _};

    use super::*;

    /// Everything `serve` needs and nothing the bare invocation needs.
    const SERVE: [&str; 11] = [
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
    ];

    /// Everything the bare stored-suite invocation needs.
    const BARE: [&str; 13] = [
        "wamn-scenario-worker",
        "--tenant",
        "tenant-a",
        "--execution-schema-template",
        "case_{ordinal}",
        "--execution-id",
        "exec-1",
        "--flow-id",
        "flow-a",
        "--flow-version",
        "1",
        "--suite-id",
        "suite-a",
    ];

    fn serve_argv() -> Vec<&'static str> {
        let mut argv = SERVE.to_vec();
        argv.push("tenant-a");
        argv
    }

    #[test]
    fn the_cli_definition_is_internally_consistent() {
        Cli::command().debug_assert();
    }

    /// The shipped `serve` invocation parses.
    ///
    /// This is the wiring no gate reached: `management_live` drives
    /// `management::serve` as a library function, so a `serve` that could not
    /// be spelled on the command line still passed every test.
    #[test]
    fn serve_parses_without_the_stored_suite_arguments() {
        let cli =
            Cli::try_parse_from(serve_argv()).expect("serve parses from its own arguments alone");
        let Some(Command::Serve(args)) = cli.command else {
            panic!("serve must select the serve subcommand");
        };
        assert_eq!(args.org, "acme");
        assert_eq!(args.project, "receiving");
        assert_eq!(args.tenant, "tenant-a");
        assert_eq!(args.system_url, "postgres://system.invalid/system");

        // `serve` reaching the surface must not depend on a stored-suite
        // argument it never reads.
        for stored_suite in [
            "--execution-schema-template",
            "--execution-id",
            "--flow-id",
            "--flow-version",
            "--suite-id",
        ] {
            let mut argv = serve_argv();
            argv.push(stored_suite);
            argv.push("anything");
            assert!(
                Cli::try_parse_from(argv).is_err(),
                "serve accepted the stored-suite argument {stored_suite}"
            );
        }
    }

    /// The historical bare invocation — the one the suiteexec gate Job uses —
    /// keeps demanding its whole argument set.
    #[test]
    fn the_bare_invocation_still_requires_its_full_argument_set() {
        let cli = Cli::try_parse_from(BARE).expect("the bare invocation parses");
        assert!(cli.command.is_none());
        let args = cli.args.expect("the bare invocation carries its arguments");
        assert_eq!(args.tenant, "tenant-a");
        assert_eq!(args.execution_id, "exec-1");
        assert_eq!(args.flow_id, "flow-a");
        assert_eq!(args.flow_version, 1);
        assert_eq!(args.suite_id, "suite-a");
        assert_eq!(args.execution_schema_template, "case_{ordinal}");
        // Defaults the gate Job relies on are unchanged.
        assert_eq!(args.source_schema, "wamn_run");

        // Dropping any required argument still refuses, one at a time.
        for required in [
            "--tenant",
            "--execution-schema-template",
            "--execution-id",
            "--flow-id",
            "--flow-version",
            "--suite-id",
        ] {
            let mut argv: Vec<&str> = Vec::new();
            let mut skip = false;
            for token in BARE {
                if token == required {
                    skip = true;
                    continue;
                }
                if skip {
                    skip = false;
                    continue;
                }
                argv.push(token);
            }
            assert!(
                Cli::try_parse_from(argv).is_err(),
                "the bare invocation accepted a missing {required}"
            );
        }

        // An invocation with neither a subcommand nor any argument is still a
        // usage error, not a silent no-op.
        assert!(Cli::try_parse_from(["wamn-scenario-worker"]).is_err());
    }
}
