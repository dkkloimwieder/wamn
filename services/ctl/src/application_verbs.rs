//! Product commands for authored application release selections.

use anyhow::Context as _;
use clap::Args;
use std::path::PathBuf;
use wamn_environment_control::environment::{self, Platform};

#[derive(Debug, Args)]
pub struct DeployArgs {
    /// The selected application's source package directory.
    #[arg(
        required_unless_present = "release",
        conflicts_with = "release",
        requires = "verified_publisher_principal"
    )]
    pub package: Option<PathBuf>,
    /// The published application release digest.
    #[arg(long)]
    pub release: Option<String>,
    /// Principal already authenticated by the publication boundary.
    #[arg(long, requires = "package")]
    pub verified_publisher_principal: Option<String>,
    /// The shared environment.k file, including schema.mode and application selections.
    #[arg(long)]
    pub env: PathBuf,
}

#[derive(Debug, Args)]
pub struct RollbackArgs {
    pub application: String,
    /// The published target release, required when the prior selection is ambiguous.
    #[arg(long)]
    pub release: Option<String>,
    /// The shared environment.k file. Rollback preserves schema.mode and other applications.
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
    let document = environment::document::EnvironmentDocument::compile(&args.env)?;
    let platform = Platform::apply_from_env()?;
    let digest = match args.package {
        Some(package) => {
            let principal = args
                .verified_publisher_principal
                .context("source deployment requires --verified-publisher-principal")?;
            let database_url = std::env::var("DATABASE_URL").context(
                "set DATABASE_URL to the PostgreSQL server for disposable build databases",
            )?;
            wamn_dev_control::source_deploy::publish(
                &platform,
                document,
                &package,
                principal,
                &database_url,
            )
            .await?
            .1
            .to_string()
        }
        None => args
            .release
            .context("select a source package or --release")?,
    };
    print_analysis(environment::deploy(&platform, &args.env, &digest).await?)
}

pub async fn rollback(args: RollbackArgs) -> anyhow::Result<()> {
    environment::document::EnvironmentDocument::compile(&args.env)?;
    let platform = Platform::apply_from_env()?;
    print_analysis(
        environment::rollback::rollback(
            &platform,
            &args.env,
            &args.application,
            args.release.as_deref(),
        )
        .await?,
    )
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
