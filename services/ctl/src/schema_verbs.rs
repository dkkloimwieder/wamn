//! Product adapters for independent schema publication, inspection, and SQL execution.

use std::path::PathBuf;

use anyhow::{Context as _, ensure};
use clap::{Args, Subcommand};
use wamn_environment_control::environment::{
    self, Platform, document::EnvironmentDocument, sql::schema,
};

#[derive(Debug, Subcommand)]
pub enum SchemaCommand {
    /// Compare selected SQL with records without writing or activating code.
    Diff(SchemaArgs),
    /// Apply selected published SQL through the existing in-cluster Job.
    Apply(ApplyArgs),
    /// Build and publish a candidate without installing it in the target database.
    Publish(PublishArgs),
    /// Replace the recorded disposable environment database and reconnect its users.
    Reset(EnvironmentArgs),
}

#[derive(Debug, Args)]
pub struct EnvironmentArgs {
    #[arg(long)]
    pub env: PathBuf,
}

#[derive(Debug, Args)]
pub struct SchemaArgs {
    /// The declared application, or a built local source directory.
    pub application: String,
    /// The shared environment.k file.
    #[arg(long)]
    pub env: PathBuf,
    /// An application release whose code does not need to be selected or installed.
    #[arg(long)]
    pub release: Option<String>,
}

#[derive(Debug, Args)]
pub struct ApplyArgs {
    #[command(flatten)]
    pub schema: SchemaArgs,
    /// Resolve one uncertain file as applied, or explicitly retry it.
    #[arg(long)]
    pub resolve: Option<wamn_package_control::apply_package::MigrationResolution>,
}

#[derive(Debug, Args)]
pub struct PublishArgs {
    pub package: PathBuf,
    #[arg(long)]
    pub env: PathBuf,
    #[arg(long)]
    pub verified_publisher_principal: String,
}

pub async fn run(command: SchemaCommand) -> anyhow::Result<()> {
    let (args, apply, resolution) = match command {
        SchemaCommand::Publish(args) => return publish(args).await,
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
    environment::runtime::resolve_platform(&mut platform, &document.org).await?;
    let (application, release, candidates) = if document
        .applications
        .contains_key(&args.application)
    {
        let (release, candidates) = schema::release_candidates(
            &platform,
            &document,
            &args.application,
            args.release.as_deref(),
        )
        .await?;
        (args.application, Some(release), candidates)
    } else {
        ensure!(
            !apply && args.release.is_none(),
            "application {} is not declared; publish a source candidate with wamn schema publish before applying it",
            args.application
        );
        ensure!(
            std::path::Path::new(&args.application).is_dir(),
            "application {} is not declared and its source package directory is missing",
            args.application
        );
        let (application, roots) = wamn_dev_control::source_deploy::sources(
            &document,
            std::path::Path::new(&args.application),
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
    if diff.requires_reset() && diff.disposable == Some(true) {
        // Resolve the complete retained set before a future reset can mutate the target.
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
        if diff.requires_reset() {
            ensure!(
                resolution.is_none(),
                "resolve uncertain SQL in migrate mode before a reset"
            );
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
        document,
        &args.package,
        args.verified_publisher_principal,
        &database_url,
    )
    .await?;
    println!(
        "{}",
        serde_json::json!({"application": application, "release": digest.as_str()})
    );
    Ok(())
}
