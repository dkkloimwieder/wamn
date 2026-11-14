//! Workflow clients of the authenticated control and application routes.

use anyhow::Context as _;
use clap::{Args, Subcommand, ValueEnum};
use std::path::PathBuf;

use crate::operation_verbs::parse_coordinate;

/// One workflow verb.
#[derive(Debug, Subcommand)]
pub enum WorkflowCommand {
    /// Author one disabled workflow version from a wiring document, and print its hash.
    Author(AuthorArgs),
    /// Enable one workflow version, and disable the version enabled before it.
    Enable(EnableArgs),
    /// Disable one workflow version. Queued and running runs finish.
    Disable(VersionArgs),
    /// Print the workflow versions of the environment, one JSON object per line.
    List(WorkflowTarget),
    /// Start the enabled version of a workflow through the host, and print its run id.
    Start(StartArgs),
    /// Hold a queued run, so no lease takes it.
    Park(RunArgs),
    /// Return a parked run to the queue.
    Release(RunArgs),
    /// Print the newest runs of the environment, one JSON object per line.
    Runs(ListArgs),
}

/// The environment selected by a workflow request.
#[derive(Debug, Args)]
pub struct WorkflowDatabase {
    #[arg(long)]
    pub environment: String,
}

/// The environment of a workflow command without a serving-release coordinate.
#[derive(Debug, Args)]
pub struct WorkflowTarget {
    #[command(flatten)]
    pub target: WorkflowDatabase,
    /// Project whose environment owns the workflow or run.
    #[arg(long)]
    pub project: String,
}

/// The identity a run executes as.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ExecutionType {
    /// The `workflow` user with `--execution-role` active.
    Role,
    /// The caller with its active role.
    Invoker,
}

#[derive(Debug, Args)]
pub struct AuthorArgs {
    /// The coordinate, `org/env/project`, whose serving release evaluates the graph conditions.
    #[arg(long)]
    pub coordinate: String,
    /// The application of the workflow, as the environment declares it.
    #[arg(long)]
    pub application: String,
    /// Compiled publication wiring from a package build. Operations must carry their sealed versions.
    #[arg(long = "document")]
    pub compiled_document: PathBuf,
    #[arg(long, value_enum)]
    pub execution_type: ExecutionType,
    /// The active role of a run. Required with `--execution-type role`.
    #[arg(long)]
    pub execution_role: Option<String>,
    /// A five-field UTC cron: the schedule task starts the workflow at each
    /// due moment. Without this or `--source-package`, the trigger is manual.
    #[arg(long, conflicts_with = "source_package")]
    pub schedule: Option<String>,
    /// The application package whose captured model starts this workflow.
    #[arg(long, requires_all = ["entity", "ops"])]
    pub source_package: Option<String>,
    #[arg(long, requires = "source_package")]
    pub entity: Option<String>,
    /// Comma-separated event operations: insert, update, delete.
    #[arg(long, value_delimiter = ',', value_parser = ["insert", "update", "delete"], requires = "source_package")]
    pub ops: Vec<String>,
    /// A registration condition over the old and new row images.
    #[arg(long, requires = "source_package")]
    pub condition: Option<String>,
}

/// One workflow version under the lifecycle lock of its coordinate.
#[derive(Debug, Args)]
pub struct VersionArgs {
    /// The coordinate, `org/env/project`, whose lifecycle lock the verb takes.
    #[arg(long)]
    pub coordinate: String,
    #[arg(long)]
    pub application: String,
    #[arg(long)]
    pub workflow_id: String,
    #[arg(long)]
    pub version: u32,
}

#[derive(Debug, Args)]
pub struct EnableArgs {
    #[command(flatten)]
    pub version: VersionArgs,
}

#[derive(Debug, Args)]
pub struct StartArgs {
    /// The base URL of the application host, such as `https://acme.example`.
    #[arg(long)]
    pub host_url: String,
    /// A bearer token of the caller: a session token or a PAT.
    #[arg(long, env = "WAMN_TOKEN", hide_env_values = true)]
    pub token: String,
    /// The application of the workflow, as the environment declares it.
    #[arg(long)]
    pub application: String,
    #[arg(long)]
    pub workflow_id: String,
    /// A repeated start with the same key and input answers the first run.
    #[arg(long)]
    pub idempotency_key: String,
    /// JSON input file passed to the workflow.
    #[arg(long)]
    pub input: PathBuf,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    #[command(flatten)]
    pub target: WorkflowTarget,
    #[arg(long)]
    pub run_id: String,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[command(flatten)]
    pub target: WorkflowTarget,
    /// The most runs to print.
    #[arg(long, default_value_t = 50)]
    pub limit: u32,
}

/// Send workflow administration to the org control service.
pub async fn run(command: WorkflowCommand) -> anyhow::Result<()> {
    use serde_json::json;
    let (operation, value) = match command {
        WorkflowCommand::Start(args) => {
            println!("{}", start(&args).await?);
            return Ok(());
        }
        WorkflowCommand::Author(args) => {
            let triple = parse_coordinate(&args.coordinate)?;
            let execution_type = match args.execution_type {
                ExecutionType::Role => "role",
                ExecutionType::Invoker => "invoker",
            };
            let document: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&args.compiled_document)?)?;
            (
                "workflow.author",
                json!({"project":triple.project,"env":triple.env,"application":args.application,
                "document":document,"execution_type":execution_type,"execution_role":args.execution_role,
                "schedule":args.schedule,"source_package":args.source_package,"entity":args.entity,"ops":args.ops,"condition":args.condition}),
            )
        }
        WorkflowCommand::Enable(args) => ("workflow.enable", version(&args.version)?),
        WorkflowCommand::Disable(args) => ("workflow.disable", version(&args)?),
        WorkflowCommand::List(args) => (
            "workflow.list",
            json!({"project":args.project,"env":args.target.environment}),
        ),
        WorkflowCommand::Park(args) => (
            "workflow.park",
            json!({"project":args.target.project,"env":args.target.target.environment,"run_id":args.run_id}),
        ),
        WorkflowCommand::Release(args) => (
            "workflow.release",
            json!({"project":args.target.project,"env":args.target.target.environment,"run_id":args.run_id}),
        ),
        WorkflowCommand::Runs(args) => (
            "workflow.runs",
            json!({"project":args.target.project,"env":args.target.target.environment,"limit":args.limit}),
        ),
    };
    let result = crate::operation_verbs::ControlHost::from_env()?
        .call(operation, &value)
        .await?;
    println!("{result}");
    Ok(())
}

fn version(args: &VersionArgs) -> anyhow::Result<serde_json::Value> {
    let triple = parse_coordinate(&args.coordinate)?;
    Ok(
        serde_json::json!({"project":triple.project,"env":triple.env,"application":args.application,"workflow_id":args.workflow_id,"version":args.version}),
    )
}

/// Call `workflow.start` on the host with the caller's token, and return the
/// run id it answers.
async fn start(args: &StartArgs) -> anyhow::Result<String> {
    let input: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&args.input).context("read the input file")?)
            .context("parse the input file")?;
    crate::component_verbs::invoke(
        &args.host_url,
        &args.token,
        "workflow.start",
        &serde_json::json!({
            "application":args.application,"workflow_id":args.workflow_id,
            "input_json":input,"idempotency_key":args.idempotency_key,
        }),
    )
    .await?["run_id"]
        .as_str()
        .map(str::to_owned)
        .context("workflow.start answers a run id")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn author_accepts_an_event_and_refuses_incomplete_or_mixed_triggers() {
        use clap::Parser as _;
        #[derive(clap::Parser)]
        struct Command {
            #[command(flatten)]
            args: AuthorArgs,
        }
        let base = [
            "workflow",
            "--coordinate",
            "acme/dev/orders",
            "--application",
            "orders",
            "--document",
            "workflow.json",
            "--execution-type",
            "invoker",
        ];
        let event = [
            "--source-package",
            "orders",
            "--entity",
            "order",
            "--ops",
            "insert,update",
            "--condition",
            "new.status != old.status",
        ];
        Command::try_parse_from(base.into_iter().chain(event)).unwrap();
        assert!(
            Command::try_parse_from(
                base.into_iter()
                    .chain(event)
                    .chain(["--schedule", "* * * * *"])
            )
            .is_err()
        );
        assert!(
            Command::try_parse_from(base.into_iter().chain(["--source-package", "orders"]))
                .is_err()
        );
        assert!(
            Command::try_parse_from(base.into_iter().chain([
                "--source-package",
                "orders",
                "--entity",
                "order",
                "--ops",
                "truncate"
            ]))
            .is_err()
        );
    }
}
