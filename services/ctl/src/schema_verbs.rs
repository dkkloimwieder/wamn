//! Product adapters for independent schema publication, inspection, and SQL execution.

use std::path::PathBuf;

use anyhow::{Context as _, ensure};
use clap::{Args, Subcommand};
use serde_json::json;
use wamn_environment_control::environment::{
    self, Platform, document::EnvironmentDocument, sql::schema,
};

use crate::operation_verbs::ControlHost;

#[derive(Debug, Subcommand)]
pub enum SchemaCommand {
    /// Compare published SQL or built source with records without changing code or data.
    Diff(SchemaArgs),
    /// Apply SQL without selecting code. Publish deployed source before the SQL Job.
    #[command(
        long_about = "Apply SQL without selecting application code. An application name uses its configured current release unless --release selects another published release. A source directory publishes first for deployed execution. With an adjacent dev.json, source SQL uses the local database directly. Migrate mode applies pending SQL. Reset mode clears application state and reapplies migrations on every schema apply in a disposable environment."
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
    /// The target environment file. An adjacent dev.json selects local execution.
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
            let configuration = args.env.with_file_name("dev.json");
            if configuration.is_file() {
                let instance =
                    wamn_dev_control::dev::target_database::reset(&configuration).await?;
                println!("{}", json!({"database_instance":instance,"result":"reset"}));
                return Ok(());
            }

            if let Some(host) = ControlHost::from_env()? {
                return host.run("schema.reset", &args.env, json!({})).await;
            }
            let platform = Platform::apply_from_env()?;
            let result = Box::pin(environment::sql::reset::reset_deployed(
                &platform, &args.env, None,
            ))
            .await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            return Ok(());
        }
        SchemaCommand::Diff(args) => (args, false),
        SchemaCommand::Apply(args) => (args, true),
    };
    let local_config = args.env.with_file_name("dev.json");
    if local_config.is_file() && std::path::Path::new(&args.application).is_dir() {
        ensure!(
            args.release.is_none(),
            "local source SQL does not select a published release"
        );
        let diffs = wamn_dev_control::dev::target_database::schema_operation(
            &local_config,
            &args.env,
            std::path::Path::new(&args.application),
            apply,
        )
        .await?;
        println!("{}", serde_json::to_string_pretty(&diffs)?);
        return Ok(());
    }
    let input = EnvironmentDocument::compile(&args.env)?;
    let source = !input.applications.contains_key(&args.application)
        && std::path::Path::new(&args.application).is_dir();
    if (apply || !source)
        && let Some(host) = ControlHost::from_env()?
    {
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
        let value = json!({"application":application, "release":release});
        return host
            .run(
                if apply { "schema.apply" } else { "schema.diff" },
                &args.env,
                value,
            )
            .await;
    }
    let mut platform = if apply {
        Platform::apply_from_env()?
    } else {
        Platform::from_env()?
    };
    let warning_application =
        (!std::path::Path::new(&args.application).is_dir()).then_some(args.application.as_str());
    let document =
        EnvironmentDocument::read_deployed(&platform, &args.env, warning_application).await?;
    let (application, release) = if apply && !document.applications.contains_key(&args.application)
    {
        ensure!(
            args.release.is_none(),
            "a source schema operation does not select --release"
        );
        let database_url = std::env::var("DATABASE_URL")
            .context("set DATABASE_URL for disposable build databases")?;
        let (application, digest) = wamn_dev_control::source_deploy::publish(
            &platform,
            &document,
            std::path::Path::new(&args.application),
            &database_url,
        )
        .await?;
        (application, Some(digest.to_string()))
    } else {
        (args.application, args.release)
    };
    environment::runtime::resolve_platform(&mut platform, &document.org).await?;
    if document.applications.contains_key(&application) {
        let diff = environment::operation::schema(
            &platform,
            &args.env,
            &application,
            release.as_deref(),
            apply,
            None,
        )
        .await?;
        println!("{}", serde_json::to_string_pretty(&diff)?);
        return Ok(());
    }
    ensure!(
        release.is_none(),
        "a source schema diff does not select --release"
    );
    ensure!(
        std::path::Path::new(&application).is_dir(),
        "application {application} is not declared and its source package directory is missing"
    );
    let (application, roots) =
        wamn_dev_control::source_deploy::sources(&document, std::path::Path::new(&application))?;
    let candidates = roots
        .into_iter()
        .map(|path| {
            schema::Candidate::source(wamn_environment_control::apply_package::PackageRoot {
                path,
                artifact_digest: None,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let state = schema::state(&platform, &document).await?;
    let diff = schema::plan(&document, &application, None, &candidates, &state).await?;
    println!("{}", serde_json::to_string_pretty(&diff)?);
    Ok(())
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
