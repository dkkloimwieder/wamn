//! Arguments and output of the `workflow` verbs: author, enable, disable and
//! list environment workflows (docs/plan/workflows.md, epic 1), and start,
//! park, release and list workflow runs through the workflow contract of
//! `wamn-workflow`.
//!
//! `author` and `enable` gate the graph against the release the environment
//! serves, derived as `env show` derives it. `enable` and `disable` take the
//! lifecycle lock of the coordinate, as `env apply` does. These three read the
//! platform inputs of `env` ([`Platform::from_env`]).

use std::path::PathBuf;

use anyhow::Context as _;
use clap::{Args, Subcommand, ValueEnum};
use tokio_postgres::NoTls;
use wamn_control::author_workflow::{AuthorWorkflowRequest, WorkflowExecution};
use wamn_control::environment::Platform;
use wamn_control::workflow_lifecycle::WorkflowVersion;
use wamn_workflow::{PostgresWorkflows, StartRequest, Trigger, Workflows as _};

use crate::env_verbs::parse_coordinate;

/// Params: `$1` tenant, `$2` environment.
const LIST_WORKFLOWS_SQL: &str = "\
SELECT application, workflow_id, version, wiring_hash, trigger, execution_type, \
       execution_role, enabled, created_by::text, created_at::text \
  FROM catalog.workflows \
 WHERE tenant_id = $1 AND environment = $2 \
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
    List(WorkflowScope),
    /// Queue one released wiring under a service principal, and print its run id.
    Start(StartArgs),
    /// Hold a queued run, so no claim takes it.
    Park(RunArgs),
    /// Return a parked run to the queue.
    Release(RunArgs),
    /// Print the newest runs of the environment, one JSON object per line.
    Runs(ListArgs),
    /// Print each declared workflow of the selected release, one JSON object per line.
    Declared(DeclaredArgs),
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
    #[command(flatten)]
    pub scope: WorkflowScope,
    /// The coordinate, `org/project/env`, whose serving release gates the graph.
    #[arg(long)]
    pub coordinate: String,
    /// Owner URL to the CONTROL database holding `wamn_run.gate_reports`.
    #[arg(long)]
    pub control_database_url: String,
    /// The application package id of the release.
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
    /// The `platform.users` id of the authoring principal.
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
    #[command(flatten)]
    pub version: VersionArgs,
    /// The `platform.users` id of the enabling principal.
    #[arg(long)]
    pub principal_id: String,
}

impl VersionArgs {
    fn workflow(&self) -> WorkflowVersion<'_> {
        WorkflowVersion {
            tenant_id: &self.scope.tenant,
            environment: &self.scope.environment,
            application: &self.application,
            workflow_id: &self.workflow_id,
            version: self.version,
        }
    }
}

#[derive(Debug, Args)]
pub struct StartArgs {
    #[command(flatten)]
    pub scope: WorkflowScope,
    /// The installed release, as its manifest digest.
    #[arg(long)]
    pub release_digest: String,
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

#[derive(Debug, Args)]
pub struct DeclaredArgs {
    #[command(flatten)]
    pub scope: WorkflowScope,
    /// Select an installed release by its manifest digest.
    #[arg(long)]
    pub release_digest: String,
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

/// Connect to the project database with project-admin authority.
async fn project(scope: &WorkflowScope) -> anyhow::Result<tokio_postgres::Client> {
    let (client, connection) = tokio_postgres::connect(&scope.admin_database_url, NoTls)
        .await
        .context("connect to the project database")?;
    tokio::spawn(connection);
    Ok(client)
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
            let document = wamn_control::author_wiring::read_wiring_document(&args.document)?;
            let platform = Platform::from_env()?;
            let triple = parse_coordinate(&args.coordinate)?;
            let digest =
                wamn_control::environment::show::serving_release(&platform, &triple).await?;
            let hash = wamn_control::author_workflow::author_workflow_in_databases(
                &args.scope.admin_database_url,
                &args.control_database_url,
                &AuthorWorkflowRequest {
                    tenant_id: &args.scope.tenant,
                    environment: &args.scope.environment,
                    application: &args.application,
                    document: &document,
                    execution,
                    created_by: &args.principal_id,
                    manifest_digest: &digest,
                },
            )
            .await?;
            println!("{hash}");
        }
        WorkflowCommand::Enable(args) => {
            let platform = Platform::from_env()?;
            let triple = parse_coordinate(&args.version.coordinate)?;
            let digest =
                wamn_control::environment::show::serving_release(&platform, &triple).await?;
            let mut client = project(&args.version.scope).await?;
            wamn_control::workflow_lifecycle::enable_workflow(
                &platform.system_database_url,
                &triple,
                &mut client,
                &args.version.workflow(),
                &args.principal_id,
                &digest,
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
                &args.workflow(),
            )
            .await?;
            println!("disabled {} version {}", args.workflow_id, args.version);
        }
        WorkflowCommand::List(scope) => {
            let mut client = project(&scope).await?;
            let transaction = client.transaction().await?;
            transaction
                .query_one(
                    "SELECT set_config('app.tenant', $1, true)",
                    &[&scope.tenant],
                )
                .await
                .context("claim the workflow tenant")?;
            for row in transaction
                .query(LIST_WORKFLOWS_SQL, &[&scope.tenant, &scope.environment])
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
        WorkflowCommand::Declared(args) => {
            let (mut client, connection) =
                tokio_postgres::connect(&args.scope.admin_database_url, NoTls)
                    .await
                    .context("connect to the project database")?;
            tokio::spawn(connection);
            let tx = client.transaction().await?;
            let bytes = wamn_control::publish_release::read_release(
                &tx,
                &args.scope.tenant,
                &args.release_digest,
            )
            .await?
            .context("the selected release is not installed")?;
            let (manifest, _) = wamn_catalog::ServingManifest::from_canonical_bytes(&bytes)?;
            tx.commit().await?;
            for schedule in manifest.workflow.schedules.values() {
                println!("{}", serde_json::to_string(schedule)?);
            }
            for (id, registration) in &manifest.workflow.registrations {
                if registration.delivery == wamn_catalog::RegistrationDelivery::Queue {
                    let (_, workflow_id) = id
                        .split_once("::")
                        .context("workflow registration has no package")?;
                    println!(
                        "{}",
                        serde_json::json!({
                            "package-id": registration.package_id,
                            "workflow-id": workflow_id,
                            "wiring-id": registration.wiring_id,
                            "wiring-version": registration.wiring_version,
                            "registration": registration,
                        })
                    );
                }
            }
        }
        WorkflowCommand::Start(args) => {
            let input =
                serde_json::from_slice(&std::fs::read(&args.input).context("read the input file")?)
                    .context("parse the input file")?;
            let run_id = workflows(&args.scope)
                .await?
                .start(&StartRequest {
                    manifest_digest: args.release_digest,
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
        WorkflowCommand::Runs(args) => {
            for run in workflows(&args.scope).await?.list(args.limit).await? {
                println!("{}", serde_json::to_string(&run)?);
            }
        }
    }
    Ok(())
}
