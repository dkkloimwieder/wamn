//! Product commands for authored application release selections.

use anyhow::Context as _;
use clap::Args;
use std::path::PathBuf;
use wamn_environment_control::environment::{self, Platform};

#[derive(Debug, Args)]
pub struct DeployArgs {
    /// The published application release digest.
    #[arg(long)]
    pub release: String,
    /// The authored environment.k file.
    #[arg(long)]
    pub env: PathBuf,
}

#[derive(Debug, Args)]
pub struct RollbackArgs {
    pub application: String,
    /// The authored environment.k file.
    #[arg(long)]
    pub env: PathBuf,
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// The authored environment.k file.
    #[arg(long)]
    pub env: PathBuf,
}

pub async fn deploy(args: DeployArgs) -> anyhow::Result<()> {
    environment::document::EnvironmentDocument::compile(&args.env)?;
    let platform = Platform::apply_from_env()?;
    print_analysis(environment::deploy(&platform, &args.env, &args.release).await?)
}

pub async fn rollback(args: RollbackArgs) -> anyhow::Result<()> {
    environment::document::EnvironmentDocument::compile(&args.env)?;
    let platform = Platform::apply_from_env()?;
    print_analysis(environment::rollback::rollback(&platform, &args.env, &args.application).await?)
}

pub async fn status(args: StatusArgs) -> anyhow::Result<()> {
    environment::document::EnvironmentDocument::compile(&args.env)?;
    let platform = Platform::from_env()?;
    let admin_url =
        std::env::var("WAMN_SYSTEM_ADMIN_URL").context("set WAMN_SYSTEM_ADMIN_URL for status")?;
    println!(
        "{}",
        serde_json::to_string_pretty(
            &environment::status::status(&platform, &args.env, &admin_url).await?
        )?
    );
    Ok(())
}

fn print_analysis(analysis: environment::analyse::Analysis) -> anyhow::Result<()> {
    for notice in analysis.notices {
        println!("{}", serde_json::to_string(&notice)?);
    }
    for line in analysis.plan {
        println!("{line}");
    }
    Ok(())
}
