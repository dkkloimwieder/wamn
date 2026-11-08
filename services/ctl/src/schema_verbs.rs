//! Product adapters for independent schema publication, inspection, and SQL execution.

use std::path::PathBuf;

use anyhow::{Context as _, ensure};
use clap::{Args, Subcommand};
use wamn_environment_control::environment::{
    self, Platform, document::EnvironmentDocument, sql::schema,
};

#[derive(Debug, Subcommand)]
pub enum SchemaCommand {
    /// Compare published SQL or built source with records without changing code or data.
    Diff(SchemaArgs),
    /// Apply SQL without selecting code. Publish deployed source before the SQL Job.
    #[command(
        long_about = "Apply SQL without selecting application code. An application name uses its configured current release unless --release selects another published release. A source directory publishes first for deployed execution. With an adjacent dev.json, source SQL uses the local database directly. Migrate mode applies pending SQL. Reset mode replaces the whole shared database only when the target is recorded as disposable and its physical schema changes."
    )]
    Apply(ApplyArgs),
    /// Build and publish a candidate without installing it in the target database.
    Publish(PublishArgs),
    /// Discard all application data in the recorded disposable environment database.
    #[command(
        long_about = "Replace the whole recorded disposable project-environment database under the same name, even when schema fingerprints match. This discards every application's data and database-local state. WAMN resolves retained sources before stopping writers. The SQL Job restores the inputs. Workloads resume with unchanged release selections. Repeat this command after interruption. The system database is outside this reset."
    )]
    Reset(EnvironmentArgs),
    /// Assert configured schema sources without executing SQL or selecting code.
    Adopt(EnvironmentArgs),
}

#[derive(Debug, Args)]
pub struct EnvironmentArgs {
    /// The shared environment.k file. An adjacent dev.json selects local adoption.
    #[arg(long)]
    pub env: PathBuf,
}

#[derive(Debug, Args)]
pub struct SchemaArgs {
    /// The declared application or source directory. Source diff needs current build output.
    pub application: String,
    /// The shared environment.k file.
    #[arg(long)]
    pub env: PathBuf,
    /// Select published application SQL instead of the configured current release. Conflicts with source input.
    #[arg(long)]
    pub release: Option<String>,
}

#[derive(Debug, Args)]
pub struct ApplyArgs {
    #[command(flatten)]
    pub schema: SchemaArgs,
    /// Resolve one uncertain file after inspection: <package>:<relative_path>=applied|retry.
    /// Applied skips SQL. Retry runs identical bytes after manual repair. No automatic repair occurs.
    #[arg(long)]
    pub resolve: Option<wamn_package_control::apply_package::MigrationResolution>,
}

#[derive(Debug, Args)]
pub struct PublishArgs {
    /// The source package to build and publish without installing SQL or selecting code.
    pub package: PathBuf,
    #[arg(long)]
    pub env: PathBuf,
}

pub async fn run(command: SchemaCommand) -> anyhow::Result<()> {
    let (args, apply, resolution) = match command {
        SchemaCommand::Publish(args) => return publish(args).await,
        SchemaCommand::Adopt(args) => {
            let configuration = args.env.with_file_name("dev.json");
            if configuration.is_file() {
                let result =
                    wamn_dev_control::dev::target_database::adopt_schema(&configuration, &args.env)
                        .await?;
                println!(
                    "{}",
                    serde_json::json!({"adopted": result, "sql_executed": false, "live_verified": false})
                );
            } else {
                let document = EnvironmentDocument::compile(&args.env)?;
                let mut platform = Platform::apply_from_env()?;
                environment::runtime::resolve_platform(&mut platform, &document.org).await?;
                environment::sql::adopt_deployed(&platform, &document).await?;
            }
            return Ok(());
        }
        SchemaCommand::Reset(args) => {
            let platform = Platform::apply_from_env()?;
            let result = Box::pin(environment::sql::reset::reset_deployed(
                &platform, &args.env, None,
            ))
            .await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            return Ok(());
        }
        SchemaCommand::Diff(args) => (args, false, None),
        SchemaCommand::Apply(args) => (args.schema, true, args.resolve),
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
            resolution.as_ref(),
        )
        .await?;
        println!("{}", serde_json::to_string_pretty(&diffs)?);
        return Ok(());
    }
    let document = EnvironmentDocument::compile(&args.env)?;
    let mut platform = if apply {
        Platform::apply_from_env()?
    } else {
        Platform::from_env()?
    };
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
    let (application, release, candidates) = if document.applications.contains_key(&application) {
        let (release, candidates) =
            schema::release_candidates(&platform, &document, &application, release.as_deref())
                .await?;
        (application, Some(release), candidates)
    } else {
        ensure!(
            release.is_none(),
            "a source schema diff does not select --release"
        );
        ensure!(
            std::path::Path::new(&application).is_dir(),
            "application {application} is not declared and its source package directory is missing"
        );
        let (application, roots) = wamn_dev_control::source_deploy::sources(
            &document,
            std::path::Path::new(&application),
        )?;
        let candidates = roots
            .into_iter()
            .map(|path| {
                schema::Candidate::source(wamn_package_control::apply_package::PackageRoot {
                    path,
                    artifact_digest: None,
                })
            })
            .collect::<Result<_, _>>()?;
        (application, None, candidates)
    };
    let state = schema::state(&platform, &document).await?;
    let diff = schema::plan(&document, &application, release, &candidates, &state)?;
    println!("{}", serde_json::to_string_pretty(&diff)?);
    if resolution.is_none() && diff.requires_reset() && diff.disposable == Some(true) {
        // Resolve the complete retained set before reset can mutate the target.
        schema::reset_candidates(&platform, &diff, candidates).await?;
    }
    if apply {
        let release = diff
            .release
            .as_deref()
            .context("schema apply needs a published release")?;
        wamn_package_control::apply_package::resolution_package(
            resolution.as_ref(),
            diff.packages
                .keys()
                .map(|id| (id.as_str(), state.migration_records(id))),
        )?;
        if resolution.is_none() && diff.requires_reset() {
            Box::pin(environment::sql::reset::reset_deployed(
                &platform,
                &args.env,
                Some((&application, release)),
            ))
            .await?;
        } else {
            environment::sql::apply_deployed(
                &platform,
                &document,
                &application,
                release,
                resolution.as_ref(),
            )
            .await?;
        }
    }
    Ok(())
}

async fn publish(args: PublishArgs) -> anyhow::Result<()> {
    let document = EnvironmentDocument::compile(&args.env)?;
    let platform = Platform::apply_from_env()?;
    let database_url =
        std::env::var("DATABASE_URL").context("set DATABASE_URL for disposable build databases")?;
    let (application, digest) = wamn_dev_control::source_deploy::publish(
        &platform,
        &document,
        &args.package,
        &database_url,
    )
    .await?;
    println!(
        "{}",
        serde_json::json!({"application": application, "release": digest.as_str()})
    );
    Ok(())
}
