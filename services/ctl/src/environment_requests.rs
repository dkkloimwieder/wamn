//! Environment lifecycle clients. Control owns all deployed inputs.
use crate::operation_verbs::{ControlHost, parse_coordinate};
use anyhow::{Context as _, ensure};
use clap::{Args, Subcommand};
use serde_json::json;
use std::{io::IsTerminal as _, path::PathBuf};

#[derive(Debug, Subcommand)]
pub enum Command {
    Apply(Apply),
    Delete(Delete),
}
#[derive(Debug, Args)]
pub struct Apply {
    pub file: PathBuf,
    #[arg(long)]
    pub application: Option<String>,
    #[arg(long)]
    pub disposable: bool,
}
#[derive(Debug, Args)]
pub struct Delete {
    pub coordinate: String,
    #[arg(long)]
    pub data: bool,
}
#[derive(Debug, Args)]
pub struct RevokePat {
    #[arg(long)]
    pub prefix: String,
}

pub async fn revoke_pat(args: RevokePat) -> anyhow::Result<()> {
    let result = ControlHost::from_env()?
        .call("pat.revoke", &json!({"token_prefix":args.prefix}))
        .await?;
    println!("{result}");
    Ok(())
}

pub async fn run(command: Command) -> anyhow::Result<()> {
    let (operation, triple, value) = match command {
        Command::Apply(args) => {
            let document =
                wamn_environment::environment::document::EnvironmentDocument::compile(&args.file)?;
            let triple = document.triple();
            (
                "environment.configure",
                triple,
                json!({"document":document,"disposable":args.disposable.then_some(true),"application":args.application}),
            )
        }
        Command::Delete(args) => {
            let triple = parse_coordinate(&args.coordinate)?;
            if args.data {
                ensure!(
                    std::io::stdin().is_terminal(),
                    "data deletion requires a terminal confirmation"
                );
                eprintln!("Type {} to delete its data:", args.coordinate);
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;
                ensure!(
                    answer.trim() == args.coordinate,
                    "environment confirmation differs"
                );
            }
            ("environment.delete", triple, json!({"data":args.data}))
        }
    };
    ControlHost::from_env()?
        .environment(operation, &triple, value)
        .await
        .context("environment lifecycle")
}
