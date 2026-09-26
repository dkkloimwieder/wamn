//! Arguments and output of the `workflow` verbs: start, park, release, and list
//! workflow runs through the workflow contract of `wamn-workflow`.

use std::path::PathBuf;

use anyhow::Context as _;
use clap::{Args, Subcommand};
use tokio_postgres::NoTls;
use wamn_workflow::{PostgresWorkflows, StartRequest, Trigger, Workflows as _};

/// One workflow verb.
#[derive(Debug, Subcommand)]
pub enum WorkflowCommand {
    /// Queue one released wiring under a service principal, and print its run id.
    Start(StartArgs),
    /// Hold a queued run, so no claim takes it.
    Park(RunArgs),
    /// Return a parked run to the queue.
    Release(RunArgs),
    /// Print the newest runs of the environment, one JSON object per line.
    List(ListArgs),
}

/// The tenant and environment every workflow verb acts in.
#[derive(Debug, Args)]
pub struct WorkflowScope {
    /// Project-admin PostgreSQL URL for the project database.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,
    /// Project run-plane schema.
    #[arg(long, default_value = "wamn_run")]
    pub schema: String,
    #[arg(long)]
    pub tenant: String,
    #[arg(long)]
    pub environment: String,
}

#[derive(Debug, Args)]
pub struct StartArgs {
    #[command(flatten)]
    pub scope: WorkflowScope,
    #[arg(long)]
    pub effective_release_id: u32,
    #[arg(long)]
    pub package_id: String,
    #[arg(long)]
    pub wiring_id: String,
    #[arg(long)]
    pub wiring_version: u32,
    /// Active projected service principal responsible for application writes.
    #[arg(long)]
    pub service_principal_id: String,
    #[arg(long)]
    pub idempotency_key: String,
    /// JSON input file passed to the wiring.
    #[arg(long)]
    pub input: PathBuf,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    #[command(flatten)]
    pub scope: WorkflowScope,
    #[arg(long)]
    pub run_id: String,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[command(flatten)]
    pub scope: WorkflowScope,
    /// The most runs to print.
    #[arg(long, default_value_t = 50)]
    pub limit: u32,
}

/// Connect with project-admin authority and bind the contract to the scope.
async fn workflows(scope: &WorkflowScope) -> anyhow::Result<PostgresWorkflows> {
    let (client, connection) = tokio_postgres::connect(&scope.admin_database_url, NoTls)
        .await
        .context("connect to the project database")?;
    tokio::spawn(connection);
    Ok(PostgresWorkflows::new(
        client,
        &scope.schema,
        &scope.tenant,
        &scope.environment,
    ))
}

/// Run one workflow verb and print its result.
pub async fn run(command: WorkflowCommand) -> anyhow::Result<()> {
    match command {
        WorkflowCommand::Start(args) => {
            let input =
                serde_json::from_slice(&std::fs::read(&args.input).context("read the input file")?)
                    .context("parse the input file")?;
            let run_id = workflows(&args.scope)
                .await?
                .start(&StartRequest {
                    effective_release_id: args.effective_release_id,
                    package_id: args.package_id,
                    wiring_id: args.wiring_id,
                    wiring_version: args.wiring_version,
                    idempotency_key: args.idempotency_key,
                    input,
                    trigger: Trigger::Automation {
                        service_principal_id: args.service_principal_id,
                    },
                })
                .await?;
            println!("{run_id}");
        }
        WorkflowCommand::Park(args) => {
            workflows(&args.scope).await?.park(&args.run_id).await?;
            println!("parked {}", args.run_id);
        }
        WorkflowCommand::Release(args) => {
            workflows(&args.scope).await?.release(&args.run_id).await?;
            println!("released {}", args.run_id);
        }
        WorkflowCommand::List(args) => {
            for run in workflows(&args.scope).await?.list(args.limit).await? {
                println!("{}", serde_json::to_string(&run)?);
            }
        }
    }
    Ok(())
}
