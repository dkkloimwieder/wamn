//! Repository task entry point.

use clap::Parser as _;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    xtask::run(xtask::Args::parse()).await
}
