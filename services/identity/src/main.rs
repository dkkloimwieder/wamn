//! Separate identity authority process.

use std::process::ExitCode;

use clap::Parser as _;
use wamn_identity::cli::{Cli, run};

fn main() -> ExitCode {
    // Load before the runtime creates threads. Existing environment values win.
    if let Err(error) = dotenvy::from_filename(".env")
        && !error.not_found()
    {
        eprintln!("identity .env file refused");
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
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
