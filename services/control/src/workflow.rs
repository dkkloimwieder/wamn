//! Workflow administration on server-selected database connections.

use anyhow::Context as _;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Connection as _, Row as _};
use wamn_identity_protocol::PrincipalId;
use wamn_workflow::workflow_lifecycle::WorkflowVersion;

use crate::execution::Execution;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    project: wamn_registry::ProjectId,
    env: wamn_registry::EnvName,
    application: Option<wamn_catalog::ApplicationName>,
    workflow_id: Option<wamn_catalog::canonical_name::CanonicalName>,
    version: Option<u32>,
}

impl Execution {
    pub(crate) async fn workflow(
        &self,
        operation: &wamn_catalog::canonical_name::CanonicalName,
        caller: &PrincipalId,
        payload: &str,
    ) -> anyhow::Result<Value> {
        let request: Request = serde_json::from_str(payload)?;
        let platform = &self.platform;
        let database = self.application_database(&request.project, &request.env)?;
        let triple = wamn_registry::Triple::new(
            self.org.clone(),
            request.env.clone(),
            request.project.clone(),
        );
        let mut application = wamn_postgres::connect(&database).await?;
        if operation == "workflow.list" {
            let mut tx = application
                .begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ")
                .await?;
            sqlx::query("INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES($1,'','','admin:workflow-list')")
                .bind(caller).execute(&mut *tx).await?;
            sqlx::query("SET TRANSACTION READ ONLY")
                .execute(&mut *tx)
                .await?;
            let rows = sqlx::query("SELECT to_jsonb(w) AS document FROM catalog.workflows w WHERE environment=$1 ORDER BY application, workflow_id, version")
                .bind(&request.env).fetch_all(&mut *tx).await?;
            let rows: Vec<Value> = rows.into_iter().map(|row| row.get("document")).collect();
            tx.commit().await?;
            return Ok(json!(rows));
        }
        let name = request
            .application
            .as_ref()
            .context("application is absent")?;
        let version = WorkflowVersion {
            environment: &request.env,
            application: name,
            workflow_id: request
                .workflow_id
                .as_ref()
                .context("workflow_id is absent")?,
            version: request.version.context("version is absent")?,
        };
        match operation.as_str() {
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
