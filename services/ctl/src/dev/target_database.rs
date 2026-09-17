//! Command-line inputs and output for development target reset.

use std::path::PathBuf;

#[derive(Debug, clap::Args)]
pub struct DevResetArgs {
    #[arg(long, value_name = "FILE")]
    config: PathBuf,
}

#[cfg(target_os = "linux")]
pub async fn reset(args: DevResetArgs) -> anyhow::Result<()> {
    let instance = wamn_control::dev::target_database::reset(&args.config).await?;
    println!("reset target-instance={instance}");
    Ok(())
}
