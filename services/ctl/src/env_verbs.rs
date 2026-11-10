//! Arguments and output of the `env` verbs
//! (docs/architecture/overview.md#environment-lifecycle). `apply` selects an
//! application and supports `--dry-run`; `rollback` selects an application; `delete` takes
//! `--data`. Every other input is a field of the environment document, or a
//! platform input that [`wamn_environment_control::environment::Platform::from_env`] reads.

use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::path::PathBuf;

use anyhow::{Context as _, bail};
use clap::{Args, Subcommand};
use wamn_control_registry::Triple;
use wamn_environment_control::environment::{self, Platform};

/// One `env` verb.
#[derive(Debug, Subcommand)]
pub enum EnvCommand {
    /// Record an environment and create roles before CNPG creates its database.
    Provision(ProvisionArgs),
    /// Initialize absent application baselines after CNPG creates the database.
    Initialize(InitializeArgs),
    /// Execute the selected schema release inside the SQL Job.
    SchemaApply(SchemaApplyArgs),
    /// Read image-owned project substrate state without changing SQL.
    ProjectSchemaDiff(ProjectSchemaArgs),
    /// Apply image-owned project substrate SQL without activating an application.
    ProjectSchemaApply(ProjectSchemaApplyArgs),
    /// Record an explicit schema assertion inside the SQL Job.
    SchemaAdopt(SchemaAdoptArgs),
    /// Resolve or restore the recorded same-name reset inputs in the SQL Job.
    Reset(ResetArgs),
    /// Move one environment toward its environment.k document.
    Apply(ApplyArgs),
    /// Write the committed integration documents of one environment.
    Connections(ConnectionsArgs),
    /// Apply one application’s previous canonical release.
    Rollback(RollbackArgs),
    /// Delete one environment. Its database stays on the cluster unless --data.
    Delete(DeleteArgs),
    /// Export the canonical environment document as KCL.
    Show(FileArgs),
    /// Print the current state of one environment as JSON.
    Status(FileArgs),
}

#[derive(Debug, Args)]
pub struct ProvisionArgs {
    /// The environment document, in KCL or compiled JSON.
    pub file: PathBuf,
    pub database: PathBuf,
    /// Write the protected database Secret for the caller to apply.
    #[arg(long)]
    pub emit_secret: PathBuf,
}

#[derive(Debug, Args)]
pub struct InitializeArgs {
    /// The environment document, in KCL or compiled JSON.
    pub file: PathBuf,
    pub database: PathBuf,
    /// The application whose absent baselines to initialize.
    #[arg(long)]
    pub application: Option<String>,
    /// The published SQL input before its code selection is recorded.
    #[arg(long, requires = "application")]
    pub release: Option<String>,
}

#[derive(Debug, Args)]
pub struct SchemaAdoptArgs {
    pub file: PathBuf,
    pub database: PathBuf,
}

#[derive(Debug, Args)]
pub struct SchemaApplyArgs {
    pub file: PathBuf,
    pub database: PathBuf,
    #[arg(long)]
    pub application: String,
    #[arg(long)]
    pub release: String,
    #[arg(long)]
    pub resolve: Option<wamn_package_control::apply_package::MigrationResolution>,
}

#[derive(Debug, Args)]
pub struct ProjectSchemaArgs {
    pub file: PathBuf,
    pub database: PathBuf,
    #[arg(long)]
    pub image: String,
}

#[derive(Debug, Args)]
pub struct ProjectSchemaApplyArgs {
    #[command(flatten)]
    pub selection: ProjectSchemaArgs,
    #[arg(long, value_name = "PATH=applied|retry", value_parser = crate::platform_verbs::platform_resolution)]
    pub resolve: Option<wamn_package_control::migrations::MigrationResolution>,
}

#[derive(Debug, Args)]
pub struct ResetArgs {
    pub file: PathBuf,
    pub database: PathBuf,
    #[arg(long, requires = "release")]
    pub application: Option<String>,
    #[arg(long, requires = "application")]
    pub release: Option<String>,
}

#[derive(Debug, Args)]
pub struct ApplyArgs {
    /// The environment.k document.
    pub file: PathBuf,
    /// Print the plan without the lock. The plan describes this moment only.
    #[arg(long)]
    pub dry_run: bool,
    /// The application whose release to analyse or apply.
    #[arg(long)]
    pub application: String,
}

#[derive(Debug, Args)]
pub struct ConnectionsArgs {
    /// The environment.k document.
    pub file: PathBuf,
    /// The directory of the integration, external system and connection
    /// documents.
    #[arg(long)]
    pub documents: PathBuf,
}

#[derive(Debug, Args)]
pub struct RollbackArgs {
    /// The published target release, required when the prior selection is ambiguous.
    #[arg(long)]
    pub release: Option<String>,
    /// The authored environment.k file.
    pub file: PathBuf,
    /// The application whose previous release to apply.
    #[arg(long)]
    pub application: String,
}

#[derive(Debug, Args)]
pub struct DeleteArgs {
    /// The coordinate, `org/project/env`.
    pub coordinate: String,
    /// Drop the database and its data too. The coordinate is read again from
    /// the terminal.
    #[arg(long)]
    pub data: bool,
}

#[derive(Debug, Args)]
pub struct FileArgs {
    /// The authored environment.k file.
    pub file: PathBuf,
}

pub async fn run(command: EnvCommand) -> anyhow::Result<()> {
    if let EnvCommand::Show(args) = &command {
        print!(
            "{}",
            environment::show::show(&Platform::from_env()?, &args.file)
                .await?
                .to_kcl()
        );
        return Ok(());
    }
    if let EnvCommand::ProjectSchemaDiff(args) = &command {
        return project_schema(args, false, None).await;
    }
    if let EnvCommand::ProjectSchemaApply(args) = &command {
        return project_schema(&args.selection, true, args.resolve.as_ref()).await;
    }
    let mut platform = match &command {
        EnvCommand::Provision(_)
        | EnvCommand::Initialize(_)
        | EnvCommand::SchemaApply(_)
        | EnvCommand::SchemaAdopt(_)
        | EnvCommand::Reset(_) => Platform::sql_from_env()?,
        EnvCommand::Apply(_) | EnvCommand::Rollback(_) => Platform::apply_from_env()?,
        _ => Platform::from_env()?,
    };
    match command {
        EnvCommand::Provision(args) => {
            let document = environment::document::EnvironmentDocument::read_deployed(
                &platform, &args.file, None,
            )
            .await?;
            let lock = sql_lock(&platform, &document, &args.database, false).await?;
            let outcome = environment::ensure::provision(
                &platform,
                &document,
                &document.policy,
                &std::env::var("WAMN_SYSTEM_ADMIN_URL")
                    .context("set WAMN_SYSTEM_ADMIN_URL for env provision")?,
                &args.emit_secret,
                None,
            )
            .await?;
            lock.release().await;
            println!("{}", serde_json::to_string(&outcome.database_cr)?);
        }
        EnvCommand::Initialize(args) => {
            let mut document = environment::document::EnvironmentDocument::read_deployed(
                &platform, &args.file, None,
            )
            .await?;
            if let Some(release) = args.release {
                wamn_package_artifact::ensure_digest(&release)?;
                let application = args
                    .application
                    .as_deref()
                    .context("initialization application")?;
                document
                    .applications
                    .get_mut(application)
                    .with_context(|| format!("application {application} is not declared"))?
                    .current = environment::document::DeclaredRelease::Digest(release);
            }
            let lock = sql_lock(&platform, &document, &args.database, false).await?;
            environment::sql::initialize(
                &platform,
                &document,
                &std::env::var("WAMN_SYSTEM_ADMIN_URL").context("SQL administrator URL")?,
                args.application.as_deref(),
            )
            .await?;
            lock.release().await;
        }
        EnvCommand::Reset(args) => {
            let document = environment::document::EnvironmentDocument::read_deployed(
                &platform, &args.file, None,
            )
            .await?;
            let expected: environment::sql::ExpectedDatabase =
                serde_json::from_slice(&std::fs::read(&args.database)?)?;
            let lock = expected.lock(&platform, &document, true).await?;
            let admin = std::env::var("WAMN_SYSTEM_ADMIN_URL").context("SQL administrator URL")?;
            if expected.pending.is_some() {
                environment::sql::reset::validate_pending_substrate(&platform, &document).await?;
                if !environment::sql::reset::needs_recreation(&platform, &document).await? {
                    environment::sql::reset::initialize(&platform, &document, &admin).await?;
                }
            } else if let Some((application, release)) =
                args.application.as_deref().zip(args.release.as_deref())
            {
                environment::sql::reset::prepare(
                    &platform,
                    &document,
                    application,
                    release,
                    &admin,
                )
                .await?;
            } else {
                environment::sql::reset::prepare_retained(&platform, &document, &admin).await?;
            }
            lock.release().await;
        }
        EnvCommand::SchemaAdopt(args) => {
            let document = environment::document::EnvironmentDocument::read_deployed(
                &platform, &args.file, None,
            )
            .await?;
            let expected: environment::sql::ExpectedDatabase =
                serde_json::from_slice(&std::fs::read(&args.database)?)?;
            let lock = expected.lock(&platform, &document, false).await?;
            let result = environment::sql::schema::adopt(
                &platform,
                &document,
                expected
                    .initialization_identity
                    .as_deref()
                    .context("adoption requires an initialization identity")?,
            )
            .await?;
            lock.release().await;
            println!(
                "{}",
                serde_json::json!({"adopted": result, "sql_executed": false, "live_verified": false})
            );
        }
        EnvCommand::SchemaApply(args) => {
            let document = environment::document::EnvironmentDocument::read_deployed(
                &platform, &args.file, None,
            )
            .await?;
            let lock = sql_lock(&platform, &document, &args.database, true).await?;
            let admin = std::env::var("WAMN_SYSTEM_ADMIN_URL").context("SQL administrator URL")?;
            environment::sql::schema::apply(
                &platform,
                &document,
                &args.application,
                &args.release,
                &admin,
                args.resolve.as_ref(),
            )
            .await?;
            lock.release().await;
            println!(
                "{}",
                serde_json::json!({"application":args.application,"release":args.release,"result":"applied"})
            );
        }
        EnvCommand::Apply(args) => {
            let analysis = if args.dry_run {
                environment::dry_run(&platform, &args.file, Some(&args.application)).await?
            } else {
                environment::apply(&platform, &args.file, &args.application).await?
            };
            for notice in &analysis.notices {
                println!("{}", serde_json::to_string(notice)?);
            }
            for line in &analysis.plan {
                println!("{line}");
            }
            if args.dry_run {
                println!("this plan describes this moment only");
            }
        }
        EnvCommand::Connections(args) => {
            let document = environment::document::EnvironmentDocument::read_deployed(
                &platform, &args.file, None,
            )
            .await?;
            for line in
                environment::connections::write(&platform, &document, &args.documents).await?
            {
                println!("{line}");
            }
        }
        EnvCommand::Rollback(args) => {
            let analysis = environment::rollback::rollback(
                &platform,
                &args.file,
                &args.application,
                args.release.as_deref(),
            )
            .await?;
            for line in &analysis.plan {
                println!("{line}");
            }
        }
        EnvCommand::Delete(args) => {
            let triple = parse_coordinate(&args.coordinate)?;
            if args.data {
                confirm_data(&args.coordinate)?;
            }
            let admin_url = std::env::var("WAMN_SYSTEM_ADMIN_URL")
                .context("set WAMN_SYSTEM_ADMIN_URL for env delete")?;
            for line in
                environment::delete::delete(&platform, &triple, args.data, &admin_url).await?
            {
                println!("{line}");
            }
        }
        EnvCommand::ProjectSchemaDiff(_) | EnvCommand::ProjectSchemaApply(_) => {
            unreachable!("project schema returns before runtime inputs")
        }
        EnvCommand::Show(_) => unreachable!("show returns before platform inputs"),
        EnvCommand::Status(args) => {
            let admin_url = std::env::var("WAMN_SYSTEM_ADMIN_URL")
                .context("set WAMN_SYSTEM_ADMIN_URL for env status")?;
            let status = environment::status::status(&mut platform, &args.file, &admin_url).await?;
            println!("{}", serde_json::to_string_pretty(&status)?);
        }
    }
    Ok(())
}

async fn project_schema(
    args: &ProjectSchemaArgs,
    apply: bool,
    resolution: Option<&wamn_package_control::migrations::MigrationResolution>,
) -> anyhow::Result<()> {
    // Substrate SQL uses no application artifact, host or broker inputs.
    let platform = Platform::project_schema_from_env()?;
    let document =
        environment::document::EnvironmentDocument::read_deployed(&platform, &args.file, None)
            .await?;
    let expected = serde_json::from_slice(&std::fs::read(&args.database)?)?;
    println!("project SQL image: {}", args.image);
    std::io::stdout().flush()?;
    environment::sql::project_schema(
        &platform,
        &document,
        environment::sql::ProjectSchemaRequest {
            expected: &expected,
            admin_url: &std::env::var("WAMN_SYSTEM_ADMIN_URL").context("SQL administrator URL")?,
            sources: &wamn_environment_control::platform_migrations::embedded_sources(
                wamn_control_provision::schema_migrations::MigrationScope::Project,
            ),
            apply,
            resolution,
        },
        std::io::stdout().lock(),
    )
    .await
}

async fn sql_lock(
    platform: &Platform,
    document: &environment::document::EnvironmentDocument,
    file: &std::path::Path,
    exclusive: bool,
) -> anyhow::Result<environment::lock::LifecycleLock> {
    let expected: environment::sql::ExpectedDatabase =
        serde_json::from_slice(&std::fs::read(file)?)?;
    let lock = expected.lock(platform, document, exclusive).await?;
    wamn_environment_control::provision_project_env::require_connected(
        lock.client(),
        &document.triple(),
    )
    .await?;
    Ok(lock)
}

/// `--data` drops the database, so the coordinate is typed again at a
/// terminal. Without a terminal the verb refuses.
fn confirm_data(coordinate: &str) -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        bail!("refused: --data reads the coordinate again from a terminal, and there is none");
    }
    eprint!("--data drops the database of {coordinate}. Type the coordinate again: ");
    std::io::stderr().flush().context("write the prompt")?;
    let mut typed = String::new();
    stdin
        .lock()
        .read_line(&mut typed)
        .context("read the coordinate")?;
    if typed.trim() != coordinate {
        bail!("refused: the coordinate typed again is not {coordinate}");
    }
    Ok(())
}

pub(crate) fn parse_coordinate(coordinate: &str) -> anyhow::Result<Triple> {
    let mut parts = coordinate.split('/');
    let (Some(org), Some(project), Some(env), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        anyhow::bail!("coordinate {coordinate:?} is not org/project/env");
    };
    wamn_control_provision::validate_project_env(org, project, env)
        .with_context(|| format!("coordinate {coordinate:?} is not valid"))?;
    Ok(Triple::new(org, project, env))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    #[test]
    fn dry_run_selects_the_named_application_through_the_analysis_owner() {
        #[derive(clap::Parser)]
        struct Command {
            #[command(subcommand)]
            command: EnvCommand,
        }
        for verb in ["apply", "rollback"] {
            assert!(Command::try_parse_from(["env", verb, "environment.k"]).is_err());
        }
        let command = Command::try_parse_from([
            "env",
            "apply",
            "environment.k",
            "--application",
            "second",
            "--dry-run",
        ])
        .unwrap();
        let EnvCommand::Apply(args) = command.command else {
            panic!("apply command")
        };
        let document = environment::document::EnvironmentDocument::from_json(
            br#"{
            "org":"acme", "project":"one", "env":"dev", "target":"cloud", "policy":"dev",
            "applications": {
                "first":{"current":"none", "hostname":"first.example", "packages":["first"]},
                "second":{"current":"none", "hostname":"second.example", "packages":["second"]}
            }
        }"#,
        )
        .unwrap();
        let selected =
            environment::analyse::select_application(&document, Some(&args.application)).unwrap();
        assert!(args.dry_run);
        assert_eq!(
            selected.applications,
            std::collections::BTreeMap::from([(
                "second".into(),
                document.applications["second"].clone()
            )])
        );
    }
}
