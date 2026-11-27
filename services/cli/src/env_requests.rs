//! Environment lifecycle clients. Control owns all deployed inputs.
use crate::operation_verbs::{ControlHost, parse_coordinate};
use anyhow::{Context as _, ensure};
use clap::{Args, Subcommand};
use serde_json::json;
use std::{io::IsTerminal as _, path::PathBuf};
use wamn_registry::{EnvName, OrgId};

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Provision an env using the admin kubeconfig for a kind platform.
    Create {
        #[arg(value_name = "ORG/ENV", value_parser = parse_env)]
        coordinate: EnvCoordinate,
        /// Mark this env as prod on first provisioning.
        #[arg(long)]
        prod: bool,
    },
    Apply(Apply),
    DeactivateProject(DeactivateProject),
}
/// An operator env coordinate without a project.
#[derive(Clone, Debug)]
pub struct EnvCoordinate {
    org: OrgId,
    env: EnvName,
}

fn parse_env(coordinate: &str) -> anyhow::Result<EnvCoordinate> {
    let (org, env) = coordinate.split_once('/').context("expected org/env")?;
    Ok(EnvCoordinate {
        org: org.parse()?,
        env: env.parse()?,
    })
}

#[derive(Debug, Args)]
pub struct Apply {
    pub file: PathBuf,
    #[arg(long)]
    pub app: Option<wamn_registry::AppName>,
    #[arg(long)]
    pub disposable: bool,
}
#[derive(Debug, Args)]
pub struct DeactivateProject {
    pub coordinate: String,
    #[arg(long)]
    pub data: bool,
    /// The full org/env/project coordinate for data deletion.
    #[arg(long, value_name = "NAME", requires = "data")]
    confirm: Option<String>,
}
#[derive(Debug, Args)]
pub struct RevokePat {
    #[arg(long)]
    pub project: wamn_registry::ProjectId,
    #[arg(long)]
    pub env: wamn_registry::EnvName,
    #[arg(long)]
    pub prefix: wamn_identity_protocol::pat_prefix::PatPrefix,
}

pub async fn revoke_pat(args: RevokePat) -> anyhow::Result<()> {
    let result = ControlHost::from_env()?
        .call(
            "pat.revoke",
            &json!({"project":args.project,"env":args.env,"token_prefix":args.prefix}),
        )
        .await?;
    println!("{result}");
    Ok(())
}

pub async fn run(command: Command) -> anyhow::Result<()> {
    let (operation, triple, value) = match command {
        Command::Create { coordinate, prod } => {
            return wamn_platform_install::env_install::create(
                &coordinate.org,
                &coordinate.env,
                prod,
            )
            .await;
        }
        Command::Apply(args) => {
            let document = wamn_env::env::document::EnvFile::compile(&args.file)?;
            let project = match &args.app {
                Some(app) => document.app_project(app)?,
                None => &document.projects[0].name,
            };
            let triple = document.triple(project);
            (
                "env.configure",
                triple,
                json!({"document":document,"disposable":args.disposable.then_some(true),"app":args.app}),
            )
        }
        Command::DeactivateProject(args) => {
            let triple = parse_coordinate(&args.coordinate)?;
            if args.data {
                let answer = if let Some(confirm) = args.confirm {
                    confirm
                } else {
                    ensure!(
                        std::io::stdin().is_terminal(),
                        "data deletion requires a terminal confirmation"
                    );
                    eprintln!("Type {} to delete its data:", args.coordinate);
                    let mut answer = String::new();
                    std::io::stdin().read_line(&mut answer)?;
                    answer
                };
                ensure!(
                    answer.trim() == args.coordinate,
                    "environment confirmation differs"
                );
            }
            ("project_env.deactivate", triple, json!({"data":args.data}))
        }
    };
    ControlHost::from_env()?
        .env(operation, &triple, value)
        .await
        .context("environment lifecycle")
}
