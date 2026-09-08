//! Separate identity authority process.

use std::process::ExitCode;

use clap::Parser as _;
use wamn_identity::cli::{Cli, run};

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
