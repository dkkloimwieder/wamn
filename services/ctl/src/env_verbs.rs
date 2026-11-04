//! Arguments and output of the `env` verbs
//! (docs/architecture/overview.md#environment-lifecycle). `apply` has one flag,
//! `--dry-run`; `rollback` one argument, `--reason`; `delete` one flag,
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
    /// Move one environment toward its environment.k document.
    Apply(ApplyArgs),
    /// Write the committed integration documents of one environment.
    Connections(ConnectionsArgs),
    /// Restore the previous intended release and web client pin of one environment.
    Rollback(RollbackArgs),
    /// Delete one environment. Its database stays on the cluster unless --data.
    Delete(DeleteArgs),
    /// Print the environment.k document the authorities hold for one environment.
    Show(ShowArgs),
    /// Print the current state of one environment as JSON.
    Status(ShowArgs),
}

#[derive(Debug, Args)]
pub struct ApplyArgs {
    /// The environment.k document.
    pub file: PathBuf,
    /// Print the plan without the lock. The plan describes this moment only.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct ConnectionsArgs {
    /// The environment.k document.
    pub file: PathBuf,
    /// The directory of the integration, external system and connection
    /// documents (platform install step 4.16.1).
    #[arg(long)]
    pub documents: PathBuf,
}

#[derive(Debug, Args)]
pub struct RollbackArgs {
    /// The coordinate, `org/project/env`.
    pub coordinate: String,
    /// Why. The Helm revision keeps it in its description.
    #[arg(long)]
    pub reason: String,
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
pub struct ShowArgs {
    /// The coordinate, `org/project/env`.
    pub coordinate: String,
}

pub async fn run(command: EnvCommand) -> anyhow::Result<()> {
    let platform = match &command {
        EnvCommand::Apply(_) | EnvCommand::Rollback(_) => Platform::apply_from_env()?,
        _ => Platform::from_env()?,
    };
    match command {
        EnvCommand::Apply(args) => {
            let analysis = if args.dry_run {
                environment::dry_run(&platform, &args.file).await?
            } else {
                environment::apply(
                    &platform,
                    &args.file,
                    &std::env::var("WAMN_SYSTEM_ADMIN_URL")
                        .context("set WAMN_SYSTEM_ADMIN_URL for ensure")?,
                )
                .await?
            };
            for line in &analysis.plan {
                println!("{line}");
            }
            if args.dry_run {
                println!("this plan describes this moment only");
            }
        }
        EnvCommand::Connections(args) => {
            let document = environment::document::EnvironmentDocument::compile(&args.file)?;
            for line in
                environment::connections::write(&platform, &document, &args.documents).await?
            {
                println!("{line}");
            }
        }
        EnvCommand::Rollback(args) => {
            let triple = parse_coordinate(&args.coordinate)?;
            let analysis = environment::rollback::rollback(
                &platform,
                &triple,
                &args.reason,
                &std::env::var("WAMN_SYSTEM_ADMIN_URL")
                    .context("set WAMN_SYSTEM_ADMIN_URL for ensure")?,
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
            for line in environment::delete::delete(&platform, &triple, args.data).await? {
                println!("{line}");
            }
        }
        EnvCommand::Show(args) => {
            let triple = parse_coordinate(&args.coordinate)?;
            print!(
                "{}",
                environment::show::show(&platform, &triple).await?.to_kcl()
            );
        }
        EnvCommand::Status(args) => {
            let triple = parse_coordinate(&args.coordinate)?;
            let status = environment::status::status(&platform, &triple).await?;
            println!("{}", serde_json::to_string_pretty(&status)?);
        }
    }
    Ok(())
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
