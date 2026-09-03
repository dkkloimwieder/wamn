//! Product command-line client for the local development loop.

#[cfg(target_os = "linux")]
use clap::{Parser, Subcommand};

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
    /// Run the package development loop once or watch for affected changes.
    Dev(wamn_ctl::dev::command::DevCommandArgs),
}

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Dev(args) => wamn_ctl::dev::command::run(args).await,
    }
}

#[cfg(not(target_os = "linux"))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("wamn dev requires Linux filesystem notifications")
}
