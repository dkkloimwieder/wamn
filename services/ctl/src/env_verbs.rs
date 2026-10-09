//! Arguments and output of the `env` verbs (docs/plan/platform-deploy.md
//! §10.1, R21). `apply` has one flag, `--dry-run`. Every other input is a
//! field of the environment document, or a platform input that
//! [`wamn_control::environment::Platform::from_env`] reads.

use std::path::PathBuf;

use anyhow::Context as _;
use clap::{Args, Subcommand};
use wamn_control::environment::{self, Platform};
use wamn_control_registry::Triple;

/// One `env` verb.
#[derive(Debug, Subcommand)]
pub enum EnvCommand {
    /// Move one environment toward its environment.k document.
    Apply(ApplyArgs),
    /// Print the environment.k document the authorities hold for one environment.
    Show(ShowArgs),
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
pub struct ShowArgs {
    /// The coordinate, `org/project/env`.
    pub coordinate: String,
}

pub async fn run(command: EnvCommand) -> anyhow::Result<()> {
    let platform = Platform::from_env()?;
    match command {
        EnvCommand::Apply(args) => {
            let analysis = if args.dry_run {
                environment::dry_run(&platform, &args.file).await?
            } else {
                environment::apply(&platform, &args.file).await?
            };
            for line in &analysis.plan {
                println!("{line}");
            }
            if args.dry_run {
                println!("this plan describes this moment only");
            }
        }
        EnvCommand::Show(args) => {
            let triple = parse_coordinate(&args.coordinate)?;
            print!(
                "{}",
                environment::show::show(&platform, &triple).await?.to_kcl()
            );
        }
    }
    Ok(())
}

fn parse_coordinate(coordinate: &str) -> anyhow::Result<Triple> {
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
