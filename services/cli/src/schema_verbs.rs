//! Product adapters for independent schema publication, inspection, and SQL execution.

use std::{io::IsTerminal as _, path::PathBuf};

use anyhow::ensure;
use clap::{Args, Subcommand};
use serde_json::json;
use wamn_environment::environment::document::EnvironmentFile;

use crate::operation_verbs::ControlHost;

#[derive(Debug, Subcommand)]
pub enum SchemaCommand {
    /// Compare published SQL or built source with records without changing code or data.
    Diff(SchemaArgs),
    /// Apply SQL for one published package digest without selecting code.
    #[command(
        long_about = "Apply pending SQL for the published package selected by --release. To clear a disposable project database, use schema reset."
    )]
    Apply(SchemaArgs),
    /// Build and publish a candidate without installing it in the target database.
    Publish(PublishArgs),
    /// Discard data for every application in the selected disposable project database.
    #[command(
        long_about = "Stop users, drop migration-owned objects, clear application catalog state, runs, queued events and authored workflows, then reapply migrations. Platform schemas, credentials and CDC objects remain. The environment must be disposable."
    )]
    Reset(ResetArgs),
}

#[derive(Debug, Args)]
pub struct ResetArgs {
    /// The project whose database will be reset.
    #[arg(long)]
    pub project: wamn_registry::ProjectId,
    /// The target environment file.
    #[arg(long)]
    pub env: PathBuf,
    /// The project ID for the database reset.
    #[arg(long, value_name = "NAME")]
    confirm: Option<String>,
}

#[derive(Debug, Args)]
pub struct SchemaArgs {
    /// The declared application or source directory. Source diff needs current build output.
    pub application: wamn_registry::ApplicationName,
    /// The target environment file.
    #[arg(long)]
    pub env: PathBuf,
    /// Select published application SQL instead of the configured current release. Conflicts with source input.
    #[arg(long)]
    pub release: Option<wamn_digest::Digest>,
}

#[derive(Debug, Args)]
pub struct PublishArgs {
    /// The source package to build and publish without installing SQL or selecting code.
    pub package: PathBuf,
    #[arg(long)]
    pub env: PathBuf,
}

pub async fn run(command: SchemaCommand) -> anyhow::Result<()> {
    let (args, apply) = match command {
        SchemaCommand::Publish(args) => return publish(args).await,
        SchemaCommand::Reset(args) => {
            let document = EnvironmentFile::compile(&args.env)?.into_project(&args.project)?;
            let answer = if let Some(confirm) = args.confirm {
                confirm
            } else {
                ensure!(
                    std::io::stdin().is_terminal(),
                    "schema reset requires a terminal confirmation"
                );
                eprintln!("Type project {} to reset its database:", args.project);
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;
                answer
            };
            ensure!(
                answer.trim() == args.project.as_str(),
                "project confirmation differs"
            );
            return ControlHost::from_env()?
                .run("schema.reset", &document.triple(), json!({}))
                .await;
        }
        SchemaCommand::Diff(args) => (args, false),
        SchemaCommand::Apply(args) => (args, true),
    };
    let file = EnvironmentFile::compile(&args.env)?;
    ensure!(
        !apply || args.release.is_some(),
        "schema apply requires --release"
    );
    let source = std::path::Path::new(args.application.as_str());
    let (triple, application, release) = match file.application_project(&args.application) {
        Ok(project) => (file.triple(project), args.application, args.release),
        Err(error) if apply || !source.is_dir() => return Err(error),
        Err(_) => {
            ensure!(
                args.release.is_none(),
                "a source schema operation does not select --release"
            );
            let application = file.only_application()?.clone();
            let document = file.into_application(&application)?;
            let (_, digest) = crate::operation_verbs::publish(&document, source).await?;
            (document.triple(), application, Some(digest))
        }
    };
    let host = ControlHost::from_env()?;
    host.run(
        if apply { "schema.apply" } else { "schema.diff" },
        &triple,
        json!({"application":application,"release":release}),
    )
    .await
}

async fn publish(args: PublishArgs) -> anyhow::Result<()> {
    let file = EnvironmentFile::compile(&args.env)?;
    let application = file.only_application()?.clone();
    let document = file.into_application(&application)?;
    let (application, digest) = crate::operation_verbs::publish(&document, &args.package).await?;
    println!("{}", json!({"application":application, "release":digest}));
    Ok(())
}
