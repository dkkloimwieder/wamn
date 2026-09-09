//! Product command-line client for the local development loop.

#[cfg(target_os = "linux")]
use anyhow::Context as _;
#[cfg(target_os = "linux")]
use clap::{Args, Parser, Subcommand};

#[cfg(target_os = "linux")]
#[derive(Debug, Parser)]
#[command(name = "wamn", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Subcommand)]
enum Command {
    /// Run the package development loop, or stand up the environment it needs.
    #[command(args_conflicts_with_subcommands = true)]
    Dev(Box<DevArgs>),
    /// Copy generated operator screens into developer-owned Rust.
    Ui(wamn_ctl::ui::UiArgs),
}

/// `wamn dev` either runs the loop from its own flags or takes `up` and stands
/// the environment up. The two are exclusive: `up` MINTS the configuration the
/// bare form's `--config` reads, so one invocation is never both.
///
/// The loop's inputs are flattened as an `Option` group rather than negated by
/// the subcommand. `subcommand_negates_reqs` is the setting that reads like the
/// right one, and it does not work here — measured on clap 4.6.6, it leaves
/// `--config` required under `wamn dev up`, whether the requirement is
/// flattened in or declared directly. The `Option` group is what actually holds
/// both halves of the rule: `wamn dev` with no subcommand still refuses without
/// `--config`, and a partly-given loop invocation still names the flag it
/// lacks.
#[cfg(target_os = "linux")]
#[derive(Debug, Args)]
struct DevArgs {
    #[command(subcommand)]
    environment: Option<DevEnvironmentCommand>,

    #[command(flatten)]
    run: Option<wamn_ctl::dev::command::DevCommandArgs>,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Subcommand)]
enum DevEnvironmentCommand {
    /// Stand up the disposable environment the loop runs against, and hold it.
    Up(wamn_ctl::dev::up::DevUpArgs),
}

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::var_os("RUST_LOG").is_some() {
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .context("read the development diagnostics filter")?;
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .init();
    }
    match Cli::parse().command {
        Command::Ui(args) => wamn_ctl::ui::run(args).await,
        Command::Dev(dev) => match dev.environment {
            Some(DevEnvironmentCommand::Up(args)) => wamn_ctl::dev::up::run(args).await,
            None => {
                let run = dev
                    .run
                    .context("wamn dev needs --config and --overlay-root, or the up subcommand")?;
                wamn_ctl::dev::command::run(run).await
            }
        },
    }
}

#[cfg(not(target_os = "linux"))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("wamn dev requires Linux filesystem notifications")
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn scaffold_parses_without_development_loop_flags() {
        let cli =
            Cli::try_parse_from(["wamn", "ui", "scaffold", "receiving", "purchase_order.get"])
                .expect("parse a standalone scaffold command");
        let Command::Ui(args) = cli.command else {
            panic!("expected the ui command");
        };
        let wamn_ctl::ui::UiCommand::Scaffold(args) = args.command;
        assert_eq!(args.package, "receiving");
        assert_eq!(args.screen.as_deref(), Some("purchase_order.get"));
    }
}
