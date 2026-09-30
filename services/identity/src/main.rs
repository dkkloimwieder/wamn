//! Separate identity authority process.

use std::process::ExitCode;

use clap::Parser as _;
use wamn_identity::cli::{Cli, run};

fn main() -> ExitCode {
    // The stderr fmt layer of the other services, with RUST_LOG over info.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
    // Load before the runtime creates threads. Existing environment values win.
    if let Err(error) = dotenvy::from_filename(".env")
        && !error.not_found()
    {
        tracing::error!("identity .env file refused");
        return ExitCode::FAILURE;
    }
    let cli = Cli::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("identity runtime initialization failed");
    match runtime.block_on(run(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("{error}");
            ExitCode::FAILURE
        }
    }
}
