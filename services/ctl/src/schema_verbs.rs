//! Product adapters for independent schema publication, inspection, and SQL execution.

use std::path::PathBuf;

use anyhow::ensure;
use clap::{Args, Subcommand};
use serde_json::json;
use wamn_environment::environment::document::EnvironmentDocument;

use crate::operation_verbs::ControlHost;

#[derive(Debug, Subcommand)]
pub enum SchemaCommand {
    /// Compare published SQL or built source with records without changing code or data.
    Diff(SchemaArgs),
    /// Apply SQL without selecting code. Publish deployed source before the SQL Job.
    #[command(
        long_about = "Apply SQL without selecting application code. An application name uses its configured current release unless --release selects another published release. A source directory publishes first for deployed execution. Migrate mode applies pending SQL. Reset mode clears application state and reapplies migrations on every schema apply in a disposable environment."
    )]
    Apply(SchemaArgs),
    /// Build and publish a candidate without installing it in the target database.
    Publish(PublishArgs),
    /// Discard all application data in the recorded disposable environment database.
    #[command(
        long_about = "Stop users, drop migration-owned objects, clear application catalog state, runs, queued events and authored workflows, then reapply migrations. Platform schemas, credentials and CDC objects remain. The environment must be disposable."
    )]
    Reset(EnvironmentArgs),
}

#[derive(Debug, Args)]
pub struct EnvironmentArgs {
    /// The target environment file.
    #[arg(long)]
    pub env: PathBuf,
}

#[derive(Debug, Args)]
pub struct SchemaArgs {
    /// The declared application or source directory. Source diff needs current build output.
    pub application: String,
    /// The target environment file.
    #[arg(long)]
    pub env: PathBuf,
    /// Select published application SQL instead of the configured current release. Conflicts with source input.
    #[arg(long)]
    pub release: Option<String>,
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
            return ControlHost::from_env()?
                .run(
                    "schema.reset",
                    &EnvironmentDocument::compile(&args.env)?.triple(),
                    json!({}),
                )
                .await;
        }
        SchemaCommand::Diff(args) => (args, false),
        SchemaCommand::Apply(args) => (args, true),
    };
    let input = EnvironmentDocument::compile(&args.env)?;
    let source = !input.applications.contains_key(&args.application)
        && std::path::Path::new(&args.application).is_dir();
    let host = ControlHost::from_env()?;
    let (application, release) = if source {
        ensure!(
            args.release.is_none(),
            "a source schema operation does not select --release"
        );
        let (application, digest) =
            crate::operation_verbs::publish(&input, std::path::Path::new(&args.application))
                .await?;
        (application, Some(digest.to_string()))
    } else {
        (args.application, args.release)
    };
    host.run(
        if apply { "schema.apply" } else { "schema.diff" },
        &input.triple(),
        json!({"application":application,"release":release}),
    )
    .await
}

async fn publish(args: PublishArgs) -> anyhow::Result<()> {
    let document = EnvironmentDocument::compile(&args.env)?;
    let (application, digest) = crate::operation_verbs::publish(&document, &args.package).await?;
    println!(
        "{}",
        json!({"application":application, "release":digest.as_str()})
    );
    Ok(())
}
