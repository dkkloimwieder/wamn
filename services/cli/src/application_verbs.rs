//! Product commands for canonical application release selections.

use anyhow::Context as _;
use clap::Args;
use serde_json::json;
use std::path::PathBuf;
use wamn_environment::environment::{self, document::EnvironmentFile};
use wamn_postgres::build_database::PostgresBuildDatabase;

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
    pub application: wamn_registry::ApplicationName,
    /// The target environment file.
    #[arg(long)]
    pub env: PathBuf,
    #[command(flatten)]
    pub(crate) describe: RefreshDescribeArgs,
}

#[derive(Debug, Args)]
pub struct RollbackArgs {
    pub application: wamn_registry::ApplicationName,
    /// Restore the content of this Helm revision.
    #[arg(long)]
    pub revision: Option<std::num::NonZeroU32>,
    /// The target environment file.
    #[arg(long)]
    pub env: PathBuf,
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Restrict status to this application's project.
    pub application: Option<wamn_registry::ApplicationName>,
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
    let file = EnvironmentFile::compile(&args.env)?;
    let triple = file.triple(file.application_project(&args.application)?);
    let digest = match args.package {
        Some(package) => {
            let document = environment::analyse::select_application(
                &file.into_project(&triple.project)?,
                Some(&args.application),
            )?;
            let published = wamn_dev::source_deploy::publish_from_env(
                &PostgresBuildDatabase,
                &crate::package_verbs::package_publisher(),
                &document,
                &package,
                args.describe.mode(false)?,
            )
            .await?;
            published.1
        }
        None => args
            .release
            .context("select a source package or --release")?,
    };
    host.run(
        "application.deploy",
        &triple,
        json!({"release":digest,"application":args.application}),
    )
    .await
}

pub async fn rollback(args: RollbackArgs) -> anyhow::Result<()> {
    let document = EnvironmentFile::compile(&args.env)?;
    let triple = document.triple(document.application_project(&args.application)?);
    ControlHost::from_env()?
        .run(
            "application.rollback",
            &triple,
            json!({"application":args.application,"revision":args.revision}),
        )
        .await
}

pub async fn status(args: StatusArgs) -> anyhow::Result<()> {
    let document = EnvironmentFile::compile(&args.env)?;
    let projects: Vec<_> = match args.application {
        Some(application) => vec![document.application_project(&application)?],
        None => document
            .projects
            .iter()
            .map(|project| &project.name)
            .collect(),
    };
    let host = ControlHost::from_env()?;
    let mut statuses = Vec::with_capacity(projects.len());
    let mut failed = false;
    for project in projects {
        let selection = json!({"project":project,"env":document.env});
        match host.call("environment.status", &selection).await {
            Ok(status) => statuses.push(json!({"project":project,"status":status})),
            Err(error) => {
                failed = true;
                statuses.push(json!({"project":project,"error":format!("{error:#}")}));
            }
        }
    }
    println!("{}", serde_json::to_string_pretty(&statuses)?);
    anyhow::ensure!(!failed, "could not read status for every selected project");
    Ok(())
}
