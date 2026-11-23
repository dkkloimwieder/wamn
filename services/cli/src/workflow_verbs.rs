//! Workflow administration through the application host.

use anyhow::Context as _;
use clap::{Args, Subcommand, ValueEnum};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use wamn_package::canonical_name::CanonicalName;

/// The application host and the administrator's bearer token.
#[derive(Debug, Args)]
pub struct Host {
    #[arg(long)]
    host_url: String,
    #[arg(long, env = "WAMN_TOKEN", hide_env_values = true)]
    token: String,
}

/// One application workflow verb.
#[derive(Debug, Subcommand)]
pub enum WorkflowCommand {
    /// Author a disabled workflow version.
    Author(Author),
    /// Enable a workflow version.
    Enable(Version),
    /// Disable a workflow version.
    Disable(Version),
    /// List the application's workflow versions.
    List(Host),
    /// Admit a run of an enabled workflow and print its run ID.
    Run(Run),
}

#[derive(Debug, Args)]
pub struct Author {
    #[command(flatten)]
    host: Host,
    #[arg(long)]
    document: PathBuf,
    /// Execution role. Required for Schedule and Data change.
    #[arg(long)]
    execution_role: Option<String>,
}

#[derive(Debug, Args)]
pub struct Version {
    #[command(flatten)]
    host: Host,
    #[arg(long)]
    workflow_id: CanonicalName,
    #[arg(id = "version", long = "version", value_name = "VERSION")]
    number: i32,
}

#[derive(Debug, Args)]
pub struct Run {
    #[command(flatten)]
    host: Host,
    #[arg(long)]
    workflow_id: CanonicalName,
    #[arg(id = "run_as", long = "run-as", value_name = "RUN_AS", value_enum)]
    identity: RunAs,
    #[arg(long)]
    idempotency_key: CanonicalName,
    #[arg(long)]
    input: PathBuf,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum RunAs {
    Admin,
    Workflow,
}

pub async fn run(command: WorkflowCommand) -> anyhow::Result<()> {
    use crate::component_verbs::invoke;
    let (host, operation, input) = match command {
        WorkflowCommand::Author(args) => {
            let graph = read_json(&args.document)?;
            (
                args.host,
                "workflow.author",
                json!({"graph": graph, "execution_role": args.execution_role}),
            )
        }
        WorkflowCommand::Enable(args) => version("workflow.enable", args),
        WorkflowCommand::Disable(args) => version("workflow.disable", args),
        WorkflowCommand::List(host) => (host, "workflow.list", json!({})),
        WorkflowCommand::Run(args) => {
            let input = read_json(&args.input)?;
            let run_as = match args.identity {
                RunAs::Admin => "admin",
                RunAs::Workflow => "workflow",
            };
            (
                args.host,
                "workflow.run",
                json!({
                    "workflow_id": args.workflow_id,
                    "run_as": run_as,
                    "idempotency_key": args.idempotency_key,
                    "input_json": input,
                }),
            )
        }
    };
    let answer = invoke(&host.host_url, &host.token, operation, &input).await?;
    if operation == "workflow.run" {
        println!(
            "{}",
            answer["run_id"]
                .as_str()
                .context("workflow.run answers a run ID")?
        );
    } else {
        println!("{answer}");
    }
    Ok(())
}

fn version(operation: &'static str, args: Version) -> (Host, &'static str, Value) {
    (
        args.host,
        operation,
        json!({"workflow_id": args.workflow_id, "version": args.number}),
    )
}

fn read_json(path: &Path) -> anyhow::Result<Value> {
    serde_json::from_slice(&std::fs::read(path)?)
        .with_context(|| format!("read JSON from {}", path.display()))
}
