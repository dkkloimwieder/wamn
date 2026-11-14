//! Workflow administration on server-selected database connections.

use anyhow::Context as _;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Connection as _, Row as _};
use wamn_identity_protocol::PrincipalId;
use wamn_workflow::PostgresWorkflows;
use wamn_workflow::author_workflow::{AuthorWorkflowRequest, WorkflowExecution, WorkflowTrigger};
use wamn_workflow::workflow_lifecycle::WorkflowVersion;

use crate::execution::Execution;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    project: String,
    env: String,
    application: Option<String>,
    workflow_id: Option<String>,
    version: Option<u32>,
    run_id: Option<String>,
    limit: Option<u32>,
    document: Option<Value>,
    execution_type: Option<String>,
    execution_role: Option<String>,
    schedule: Option<String>,
    source_package: Option<String>,
    entity: Option<String>,
    #[serde(default)]
    ops: Vec<wamn_event_wire::Op>,
    condition: Option<String>,
}

impl Execution {
    pub(crate) async fn workflow(
        &self,
        operation: &str,
        caller: &PrincipalId,
        payload: &str,
    ) -> anyhow::Result<Value> {
        let request: Request = serde_json::from_str(payload)?;
        let platform = &self.platform;
        let database = self.application_database(&request.project, &request.env)?;
        let triple = wamn_registry::Triple::new(&self.org, request.env.as_str(), &request.project);
        let key = wamn_registry::project_env_key(&self.org, &request.project, &request.env);
        if matches!(
            operation,
            "workflow.park" | "workflow.release" | "workflow.runs"
        ) {
            let workflows = PostgresWorkflows::new(
                wamn_postgres::connect(&database).await?,
                "wamn_run",
                key,
                &request.env,
            );
            return Ok(match operation {
                "workflow.park" => {
                    workflows
                        .park(request.run_id.as_deref().context("run_id is absent")?)
                        .await?;
                    json!({"status":"parked"})
                }
                "workflow.release" => {
                    workflows
                        .release(request.run_id.as_deref().context("run_id is absent")?)
                        .await?;
                    json!({"status":"queued"})
                }
                _ => serde_json::to_value(workflows.list(request.limit.unwrap_or(50)).await?)?,
            });
        }
        let mut application = wamn_postgres::connect(&database).await?;
        if operation == "workflow.list" {
            let mut tx = application.begin().await?;
            sqlx::query("SELECT set_config('app.project_env_key', $1, true)")
                .bind(&key)
                .execute(&mut *tx)
                .await?;
            let rows = sqlx::query("SELECT to_jsonb(w) AS document FROM catalog.workflows w WHERE project_env_key=$1 AND environment=$2 ORDER BY application, workflow_id, version")
                .bind(&key).bind(&request.env).fetch_all(&mut *tx).await?;
            let rows: Vec<Value> = rows.into_iter().map(|row| row.get("document")).collect();
            tx.commit().await?;
            return Ok(json!(rows));
        }
        let name = request
            .application
            .as_deref()
            .context("application is absent")?;
        let actor = caller.to_string();
        if operation == "workflow.author" {
            anyhow::ensure!(
                request.source_package.is_some()
                    || (request.entity.is_none()
                        && request.ops.is_empty()
                        && request.condition.is_none()),
                "event fields require a source package"
            );
            anyhow::ensure!(
                request.source_package.is_none() || !request.ops.is_empty(),
                "an event trigger requires operations"
            );
            let document = wamn_catalog::WiringDocument::parse(
                request.document.as_ref().context("document is absent")?,
            )?;
            let execution = match (
                request.execution_type.as_deref(),
                request.execution_role.as_deref(),
            ) {
                (Some("invoker"), None) => WorkflowExecution::Invoker,
                (Some("role"), Some(role)) => WorkflowExecution::Role(role),
                _ => return Err(anyhow::anyhow!("invalid workflow execution authority")),
            };
            let trigger = match (
                request.schedule.as_deref(),
                request.source_package.as_deref(),
            ) {
                (None, None) => WorkflowTrigger::Manual,
                (Some(schedule), None) => WorkflowTrigger::Schedule(schedule),
                (None, Some(source_package)) => WorkflowTrigger::Event {
                    source_package,
                    entity: request.entity.as_deref().context("entity is absent")?,
                    ops: &request.ops,
                    condition: request.condition.as_deref(),
                },
                _ => return Err(anyhow::anyhow!("a workflow has one trigger")),
            };
            let (digest, declared) =
                wamn_environment::environment::show::serving_application(platform, &triple, name)
                    .await?;
            let hash = wamn_workflow::author_workflow::author_workflow_in_databases(
                &database,
                &AuthorWorkflowRequest {
                    org: &self.org,
                    project: &request.project,
                    environment: &request.env,
                    application: name,
                    declared: &declared,
                    document: &document,
                    trigger,
                    execution,
                    created_by: &actor,
                    manifest_digest: &digest,
                },
            )
            .await?;
            return Ok(json!({"wiring_hash":hash}));
        }
        let version = WorkflowVersion {
            org: &self.org,
            project: &request.project,
            environment: &request.env,
            application: name,
            workflow_id: request
                .workflow_id
                .as_deref()
                .context("workflow_id is absent")?,
            version: request.version.context("version is absent")?,
        };
        match operation {
            "workflow.enable" => {
                let (digest, declared) = wamn_environment::environment::show::serving_application(
                    platform, &triple, name,
                )
                .await?;
                wamn_workflow::workflow_lifecycle::enable_workflow(
                    &platform.system_database_url,
                    &triple,
                    &mut application,
                    &version,
                    &actor,
                    &digest,
                    &declared,
                    platform.events.as_ref(),
                )
                .await?;
            }
            "workflow.disable" => {
                wamn_workflow::workflow_lifecycle::disable_workflow(
                    &platform.system_database_url,
                    &triple,
                    &mut application,
                    &version,
                )
                .await?;
            }
            _ => return Err(anyhow::anyhow!("unknown workflow operation")),
        }
        Ok(json!({"workflow_id":version.workflow_id,"version":version.version}))
    }
}
