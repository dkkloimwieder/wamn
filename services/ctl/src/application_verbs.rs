//! Product commands for canonical application release selections.

use anyhow::Context as _;
use clap::Args;
use serde_json::json;
use std::path::PathBuf;
use wamn_environment::environment::{self, Platform};

use crate::operation_verbs::ControlHost;

#[derive(Debug, Args)]
pub struct DeployArgs {
    /// The selected application's source package directory.
    #[arg(required_unless_present = "release", conflicts_with = "release")]
    pub package: Option<PathBuf>,
    /// The published application release digest.
    #[arg(long)]
    pub release: Option<String>,
    /// The application to deploy.
    #[arg(long)]
    pub application: String,
    /// The target environment file. Apply application SQL before deploying code.
    #[arg(long)]
    pub env: PathBuf,
}

#[derive(Debug, Args)]
pub struct RollbackArgs {
    pub application: String,
    /// The published target release, to select an explicit release.
    #[arg(long)]
    pub release: Option<String>,
    /// The target environment file. Rollback preserves schema.mode and other applications.
    #[arg(long)]
    pub env: PathBuf,
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// The target environment file.
    #[arg(long)]
    pub env: PathBuf,
}

pub async fn deploy(args: DeployArgs) -> anyhow::Result<()> {
    if let Some(host) = ControlHost::from_env()? {
        let digest = match args.package {
            Some(package) => {
                let document = environment::analyse::select_application(
                    &environment::document::EnvironmentDocument::compile(&args.env)?,
                    Some(&args.application),
                )?;
                crate::operation_verbs::publish(&document, &package)
                    .await?
                    .1
                    .to_string()
            }
            None => args
                .release
                .context("select a source package or --release")?,
        };
        return host
            .run(
                "application.deploy",
                &environment::document::EnvironmentDocument::compile(&args.env)?.triple(),
                json!({"release":digest,"application":args.application}),
            )
            .await;
    }
    let platform = Platform::apply_from_env()?;
    let document = environment::document::EnvironmentDocument::read(
        &platform,
        &environment::document::EnvironmentDocument::compile(&args.env)?.triple(),
    )
    .await?;
    let document = environment::analyse::select_application(&document, Some(&args.application))?;
    let digest = match args.package {
        Some(package) => {
            let database_url = std::env::var("DATABASE_URL").context(
                "set DATABASE_URL to the PostgreSQL server for disposable build databases",
            )?;
            wamn_dev_control::source_deploy::publish(&platform, &document, &package, &database_url)
                .await?
                .1
                .to_string()
        }
        None => args
            .release
            .context("select a source package or --release")?,
    };
    print_analysis(
        environment::deploy(&platform, &document.triple(), &args.application, &digest).await?,
    )
}

pub async fn rollback(args: RollbackArgs) -> anyhow::Result<()> {
    if let Some(host) = ControlHost::from_env()? {
        return host
            .run(
                "application.rollback",
                &environment::document::EnvironmentDocument::compile(&args.env)?.triple(),
                json!({"application":args.application, "release":args.release}),
            )
            .await;
    }
    let platform = Platform::apply_from_env()?;
    print_analysis(
        environment::rollback::rollback(
            &platform,
            &environment::document::EnvironmentDocument::compile(&args.env)?.triple(),
            &args.application,
            args.release.as_deref(),
        )
        .await?,
    )
}

pub async fn status(args: StatusArgs) -> anyhow::Result<()> {
    let mut platform = Platform::from_env()?;
    let admin_url =
        std::env::var("WAMN_SYSTEM_ADMIN_URL").context("set WAMN_SYSTEM_ADMIN_URL for status")?;
    println!(
        "{}",
        serde_json::to_string_pretty(
            &environment::status::status(
                &mut platform,
                &environment::document::EnvironmentDocument::compile(&args.env)?.triple(),
                &admin_url
            )
            .await?
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
