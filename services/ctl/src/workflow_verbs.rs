//! Arguments and output of the `workflow` verbs: author, enable, disable and
//! list environment workflows
//! (docs/architecture/execution.md#environment-workflows), start a run through
//! the host route `workflow.start`, and park, release and list workflow runs
//! through the workflow contract of `wamn-workflow`.
//!
//! `author` and `enable` gate the graph against the release the environment
//! serves, derived as `env show` derives it. `enable` and `disable` take the
//! lifecycle lock of the coordinate, as `env apply` does. These three read the
//! platform inputs of `env` ([`Platform::from_env`]).

use std::path::PathBuf;

use anyhow::Context as _;
use clap::{Args, Subcommand, ValueEnum};
use tokio_postgres::NoTls;
use wamn_control::author_workflow::{AuthorWorkflowRequest, WorkflowExecution, WorkflowTrigger};
use wamn_control::environment::Platform;
use wamn_control::workflow_lifecycle::WorkflowVersion;
use wamn_workflow::{PostgresWorkflows, Workflows as _};

use crate::env_verbs::parse_coordinate;

/// Params: `$1` `env_key`, `$2` environment.
const LIST_WORKFLOWS_SQL: &str = "\
SELECT application, workflow_id, version, wiring_hash, trigger, execution_type, \
       execution_role, enabled, created_by::text, created_at::text, schedule, source_package, entity, ops, condition, enable_sequence \
  FROM catalog.workflows \
 WHERE env_key = $1 AND environment = $2 \
 ORDER BY application, workflow_id, version";

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
    /// Hold a queued run, so no claim takes it.
    Park(RunArgs),
    /// Return a parked run to the queue.
    Release(RunArgs),
    /// Print the newest runs of the environment, one JSON object per line.
    Runs(ListArgs),
}

/// The database and environment every workflow verb acts in.
#[derive(Debug, Args)]
pub struct WorkflowScope {
    /// Project-admin PostgreSQL URL for the project database.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,
    /// Project run-plane schema.
    #[arg(long, default_value = "wamn_run")]
    pub schema: String,
    #[arg(long)]
    pub environment: String,
}

/// The environment of a workflow command without a serving-release coordinate.
#[derive(Debug, Args)]
pub struct WorkflowTarget {
    #[command(flatten)]
    pub scope: WorkflowScope,
    #[arg(long)]
    pub org: String,
    /// Project whose environment owns the workflow or run.
    #[arg(long)]
    pub project: String,
}

impl WorkflowTarget {
    fn env_key(&self) -> String {
        wamn_control_registry::project_env_key(&self.org, &self.project, &self.scope.environment)
    }
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
    /// Read the local host's release facts from this dev root instead of Helm.
    #[arg(long)]
    pub dev_root: Option<PathBuf>,
    #[command(flatten)]
    pub scope: WorkflowScope,
    /// The coordinate, `org/project/env`, whose serving release gates the graph.
    #[arg(long)]
    pub coordinate: String,
    /// Owner URL to the CONTROL database holding `wamn_run.gate_reports`.
    #[arg(long)]
    pub control_database_url: String,
    /// The application of the workflow, as the environment declares it.
    #[arg(long)]
    pub application: String,
    /// The wiring document. Its `wiring-id` and `version` name the workflow version.
    #[arg(long)]
    pub document: PathBuf,
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
    /// The `platform."user"` id of the authoring principal.
    #[arg(long)]
    pub principal_id: String,
}

/// One workflow version under the lifecycle lock of its coordinate.
#[derive(Debug, Args)]
pub struct VersionArgs {
    #[command(flatten)]
    pub scope: WorkflowScope,
    /// The coordinate, `org/project/env`, whose lifecycle lock the verb takes.
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
    /// Read the local host's release facts from this dev root instead of Helm.
    #[arg(long)]
    pub dev_root: Option<PathBuf>,
    #[command(flatten)]
    pub version: VersionArgs,
    /// The `platform."user"` id of the enabling principal.
    #[arg(long)]
    pub principal_id: String,
}

impl VersionArgs {
    fn workflow<'a>(
        &'a self,
        coordinate: &'a wamn_control_registry::Triple,
    ) -> WorkflowVersion<'a> {
        WorkflowVersion {
            org: &coordinate.org,
            project: &coordinate.project,
            environment: &self.scope.environment,
            application: &self.application,
            workflow_id: &self.workflow_id,
            version: self.version,
        }
    }
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
    pub scope: WorkflowTarget,
    #[arg(long)]
    pub run_id: String,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[command(flatten)]
    pub scope: WorkflowTarget,
    /// The most runs to print.
    #[arg(long, default_value_t = 50)]
    pub limit: u32,
}

async fn workflows(scope: &WorkflowTarget) -> anyhow::Result<PostgresWorkflows> {
    let (client, connection) = tokio_postgres::connect(&scope.scope.admin_database_url, NoTls)
        .await
        .context("connect to the project database")?;
    tokio::spawn(connection);
    Ok(PostgresWorkflows::new(
        client,
        &scope.scope.schema,
        scope.env_key(),
        &scope.scope.environment,
    ))
}

/// Connect to the project database with project-admin authority.
async fn project(scope: &WorkflowScope) -> anyhow::Result<tokio_postgres::Client> {
    let (client, connection) = tokio_postgres::connect(&scope.admin_database_url, NoTls)
        .await
        .context("connect to the project database")?;
    tokio::spawn(connection);
    Ok(client)
}

async fn serving_facts(
    triple: &wamn_control_registry::Triple,
    dev_root: Option<&std::path::Path>,
) -> anyhow::Result<(String, wamn_catalog::EnvironmentApplications)> {
    if let Some(root) = dev_root {
        let release =
            wamn_runtime::local_application::read_local_release(&root.join("local-artifacts"))?;
        return Ok((release.manifest_digest.to_string(), release.applications));
    }
    let platform = Platform::from_env()?;
    let digest = wamn_control::environment::show::serving_release(&platform, triple).await?;
    let applications =
        wamn_control::environment::show::serving_applications(&platform, triple).await?;
    Ok((digest, applications))
}

/// Run one workflow verb and print its result.
pub async fn run(command: WorkflowCommand) -> anyhow::Result<()> {
    match command {
        WorkflowCommand::Author(args) => {
            let execution = match (args.execution_type, args.execution_role.as_deref()) {
                (ExecutionType::Role, Some(role)) => WorkflowExecution::Role(role),
                (ExecutionType::Invoker, None) => WorkflowExecution::Invoker,
                (ExecutionType::Role, None) => {
                    anyhow::bail!("--execution-type role needs --execution-role")
                }
                (ExecutionType::Invoker, Some(_)) => {
                    anyhow::bail!("--execution-type invoker takes no --execution-role")
                }
            };
            let ops: Vec<_> = serde_json::from_value(serde_json::json!(args.ops))?;
            let trigger = if let Some(source_package) = args.source_package.as_deref() {
                WorkflowTrigger::Event {
                    source_package,
                    entity: args
                        .entity
                        .as_deref()
                        .context("--source-package needs --entity")?,
                    ops: &ops,
                    condition: args.condition.as_deref(),
                }
            } else {
                args.schedule
                    .as_deref()
                    .map_or(WorkflowTrigger::Manual, WorkflowTrigger::Schedule)
            };
            let document = wamn_control::author_workflow::read_wiring_document(&args.document)?;
            let triple = parse_coordinate(&args.coordinate)?;
            let (digest, declared) = serving_facts(&triple, args.dev_root.as_deref()).await?;
            let hash = wamn_control::author_workflow::author_workflow_in_databases(
                &args.scope.admin_database_url,
                &args.control_database_url,
                &AuthorWorkflowRequest {
                    org: &triple.org,
                    project: &triple.project,
                    environment: &args.scope.environment,
                    application: &args.application,
                    declared: &declared,
                    document: &document,
                    trigger,
                    execution,
                    created_by: &args.principal_id,
                    manifest_digest: &digest,
                },
            )
            .await?;
            println!("{hash}");
        }
        WorkflowCommand::Enable(args) => {
            let (system_database_url, events) = if args.dev_root.is_some() {
                (
                    std::env::var("WAMN_CONTROL_DATABASE_URL")
                        .context("set WAMN_CONTROL_DATABASE_URL")?,
                    wamn_control::environment::event_broker_from_env()?,
                )
            } else {
                let platform = Platform::from_env()?;
                (platform.system_database_url, platform.events)
            };
            let triple = parse_coordinate(&args.version.coordinate)?;
            let (digest, declared) = serving_facts(&triple, args.dev_root.as_deref()).await?;
            let mut client = project(&args.version.scope).await?;
            wamn_control::workflow_lifecycle::enable_workflow(
                &system_database_url,
                &triple,
                &mut client,
                &args.version.workflow(&triple),
                &args.principal_id,
                &digest,
                &declared,
                events.as_ref(),
            )
            .await?;
            println!(
                "enabled {} version {}",
                args.version.workflow_id, args.version.version
            );
        }
        WorkflowCommand::Disable(args) => {
            let platform = Platform::from_env()?;
            let triple = parse_coordinate(&args.coordinate)?;
            let mut client = project(&args.scope).await?;
            wamn_control::workflow_lifecycle::disable_workflow(
                &platform.system_database_url,
                &triple,
                &mut client,
                &args.workflow(&triple),
            )
            .await?;
            println!("disabled {} version {}", args.workflow_id, args.version);
        }
        WorkflowCommand::List(scope) => {
            let mut client = project(&scope.scope).await?;
            let env_key = scope.env_key();
            let transaction = client.transaction().await?;
            transaction
                .query_one("SELECT set_config('app.env_key', $1, true)", &[&env_key])
                .await
                .context("claim the workflow tenant")?;
            for row in transaction
                .query(LIST_WORKFLOWS_SQL, &[&env_key, &scope.scope.environment])
                .await
                .context("read the workflow versions")?
            {
                println!(
                    "{}",
                    serde_json::json!({
                        "application": row.get::<_, String>(0),
                        "workflow-id": row.get::<_, String>(1),
                        "version": row.get::<_, i32>(2),
                        "wiring-hash": row.get::<_, String>(3),
                        "trigger": row.get::<_, String>(4),
                        "schedule": row.get::<_, Option<String>>(10),
                        "source-package": row.get::<_, Option<String>>(11),
                        "entity": row.get::<_, Option<String>>(12),
                        "ops": row.get::<_, Option<Vec<String>>>(13),
                        "condition": row.get::<_, Option<String>>(14),
                        "enable-sequence": row.get::<_, Option<i64>>(15),
                        "execution-type": row.get::<_, String>(5),
                        "execution-role": row.get::<_, Option<String>>(6),
                        "enabled": row.get::<_, bool>(7),
                        "created-by": row.get::<_, String>(8),
                        "created-at": row.get::<_, String>(9),
                    })
                );
            }
            transaction.commit().await?;
        }

        WorkflowCommand::Start(args) => {
            println!("{}", start(&args).await?);
        }
        WorkflowCommand::Park(args) => {
            workflows(&args.scope).await?.park(&args.run_id).await?;
            println!("parked {}", args.run_id);
        }
        WorkflowCommand::Release(args) => {
            workflows(&args.scope).await?.release(&args.run_id).await?;
            println!("released {}", args.run_id);
        }
        WorkflowCommand::Runs(args) => {
            for run in workflows(&args.scope).await?.list(args.limit).await? {
                println!("{}", serde_json::to_string(&run)?);
            }
        }
    }
    Ok(())
}

/// The path of the host route `workflow.start`.
fn start_route_path() -> anyhow::Result<String> {
    let (_, attachment) = wamn_catalog::HostRouteSet::Application
        .attachments()
        .find(|(_, attachment)| {
            attachment.route.handler == wamn_catalog::HostHandler::WorkflowStart
        })
        .context("the application host routes serve workflow.start")?;
    attachment.definition["route"]["path"]
        .as_str()
        .map(str::to_owned)
        .context("workflow.start names its path")
}

/// The one request element of `workflow.start`, as a generated client sends a
/// write: the request id and the input under `value`.
fn start_request(args: &StartArgs, input: &serde_json::Value) -> serde_json::Value {
    serde_json::json!([{
        "request_id": args.idempotency_key,
        "value": {
            "application": args.application,
            "workflow_id": args.workflow_id,
            "input_json": input,
            "idempotency_key": args.idempotency_key,
        },
    }])
}

/// Call `workflow.start` on the host with the caller's token, and return the
/// run id it answers.
async fn start(args: &StartArgs) -> anyhow::Result<String> {
    let input = serde_json::from_slice(&std::fs::read(&args.input).context("read the input file")?)
        .context("parse the input file")?;
    let url = format!(
        "{}{}",
        args.host_url.trim_end_matches('/'),
        start_route_path()?
    );
    let response = reqwest::Client::new()
        .post(&url)
        .bearer_auth(&args.token)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(start_request(args, &input).to_string())
        .send()
        .await
        .with_context(|| format!("call workflow.start at {url}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .context("read the workflow.start answer")?;
    anyhow::ensure!(
        status.is_success(),
        "workflow.start answered HTTP {status}: {body}"
    );
    let [request_element]: [serde_json::Value; 1] =
        serde_json::from_str(&body).context("workflow.start answers one request element")?;
    if let Some(error) = request_element.get("error") {
        anyhow::bail!("workflow.start refused: {error}");
    }
    request_element["value"]["run_id"]
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
            "--admin-database-url",
            "postgres://localhost/project",
            "--environment",
            "dev",
            "--coordinate",
            "acme/orders/dev",
            "--control-database-url",
            "postgres://localhost/control",
            "--application",
            "orders",
            "--document",
            "workflow.json",
            "--execution-type",
            "invoker",
            "--principal-id",
            "alice",
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
        let command = Command::try_parse_from(base.into_iter().chain(event)).unwrap();
        assert_eq!(command.args.ops, ["insert", "update"]);
        assert_eq!(command.args.source_package.as_deref(), Some("orders"));
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

    #[tokio::test]
    async fn dev_root_supplies_the_host_release_without_helm() {
        let directory =
            std::env::temp_dir().join(format!("wamn-workflow-dev-root-{}", std::process::id()));
        let artifacts = directory.join("local-artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        let digest = format!("sha256:{}", "a".repeat(64));
        let applications = std::collections::BTreeMap::from([(
            "overlay".to_owned(),
            wamn_catalog::ApplicationDefinition {
                connectors: std::collections::BTreeMap::new(),
                connections: std::collections::BTreeMap::new(),
                packages: ["base".to_owned(), "overlay".to_owned()].into(),
            },
        )]);
        std::fs::write(
            artifacts.join(wamn_runtime::local_application::LOCAL_RELEASE_FILE),
            serde_json::to_vec(&wamn_runtime::local_application::LocalRelease {
                manifest_digest: wamn_catalog::ManifestDigest::parse(&digest).unwrap(),
                applications: applications.clone(),
            })
            .unwrap(),
        )
        .unwrap();
        let facts = serving_facts(
            &wamn_control_registry::Triple::new("org", "project", "dev"),
            Some(&directory),
        )
        .await
        .unwrap();
        assert_eq!(facts, (digest, applications));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn start_calls_the_host_route_with_no_user() {
        assert_eq!(start_route_path().unwrap(), "/wamn_control/workflow/start");
        let args = StartArgs {
            host_url: "https://acme.example".to_owned(),
            token: "token".to_owned(),
            application: "orders".to_owned(),
            workflow_id: "nightly".to_owned(),
            idempotency_key: "batch-42".to_owned(),
            input: PathBuf::from("input.json"),
        };
        assert_eq!(
            start_request(&args, &serde_json::json!({"day": 1})),
            serde_json::json!([{
                "request_id": "batch-42",
                "value": {
                    "application": "orders",
                    "workflow_id": "nightly",
                    "input_json": {"day": 1},
                    "idempotency_key": "batch-42",
                },
            }])
        );
    }
}
