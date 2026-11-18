//! Product commands for canonical application release selections.

use anyhow::Context as _;
use clap::Args;
use serde_json::json;
use std::path::PathBuf;
use wamn_environment::environment;

use crate::build_verbs::RefreshDescribeArgs;
use crate::operation_verbs::ControlHost;

#[derive(Debug, Args)]
pub struct DeployArgs {
    /// The selected application's source package directory.
    #[arg(required_unless_present = "release", conflicts_with = "release")]
    pub package: Option<PathBuf>,
    /// The published application release digest.
    #[arg(long)]
    pub release: Option<wamn_digest::Digest>,
    /// The application to deploy.
    #[arg(long)]
    pub application: wamn_catalog::ApplicationName,
    /// The target environment file. Apply application SQL before deploying code.
    #[arg(long)]
    pub env: PathBuf,
    #[command(flatten)]
    pub(crate) describe: RefreshDescribeArgs,
}

#[derive(Debug, Args)]
pub struct RollbackArgs {
    pub application: wamn_catalog::ApplicationName,
    /// The published target release, to select an explicit release.
    #[arg(long)]
    pub release: Option<wamn_digest::Digest>,
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
    anyhow::ensure!(
        args.package.is_some() || !args.describe.refresh_describe,
        "--refresh-describe requires a source package"
    );
    let host = ControlHost::from_env()?;
    let digest = match args.package {
        Some(package) => {
            let document = environment::analyse::select_application(
                &environment::document::EnvironmentDocument::compile(&args.env)?,
                Some(&args.application),
            )?;
            let published = wamn_dev::source_deploy::publish_from_env(&document, &package, args.describe.mode(false)?).await?;
            published.1
        }
        None => args
            .release
            .context("select a source package or --release")?,
    };
    host.run(
        "application.deploy",
        &environment::document::EnvironmentDocument::compile(&args.env)?.triple(),
        json!({"release":digest,"application":args.application}),
    )
    .await
}

pub async fn rollback(args: RollbackArgs) -> anyhow::Result<()> {
    ControlHost::from_env()?
        .run(
            "application.rollback",
            &environment::document::EnvironmentDocument::compile(&args.env)?.triple(),
            json!({"application":args.application,"release":args.release}),
        )
        .await
}

pub async fn status(args: StatusArgs) -> anyhow::Result<()> {
    let target = environment::document::EnvironmentDocument::compile(&args.env)?.triple();
    let status = ControlHost::from_env()?
        .call(
            "environment.status",
            &json!({"project":target.project,"env":target.env}),
        )
        .await?;
    println!("{}", serde_json::to_string_pretty(&status)?);
    Ok(())
}
