//! Author and enable environment workflows for live fixtures.

use wamn_control::author_workflow::{AuthorWorkflowRequest, author_workflow};
use wamn_control::environment::EventBroker;
use wamn_control::workflow_lifecycle::{WorkflowVersion, enable_workflow};
use wamn_control_registry::Triple;

/// Use the same authorship and enable boundaries as the control verbs.
pub async fn author_and_enable(
    system_url: &str,
    project_url: &str,
    scope: &Triple,
    request: &AuthorWorkflowRequest<'_>,
    events: Option<&EventBroker>,
) -> anyhow::Result<()> {
    let (control, connection) = tokio_postgres::connect(system_url, tokio_postgres::NoTls).await?;
    let control_task = tokio::spawn(connection);
    let (mut project, connection) =
        tokio_postgres::connect(project_url, tokio_postgres::NoTls).await?;
    let project_task = tokio::spawn(connection);
    let result = async {
        project.execute(
            "INSERT INTO platform.user_role (user_id, role_name) VALUES ($1::text::uuid, 'admin') ON CONFLICT DO NOTHING",
            &[&request.created_by],
        ).await?;
        // The graph still passes the release gate in author_workflow.
        control.execute("SELECT set_config('app.tenant', $1, false)", &[&request.tenant_id]).await?;
        control.execute(
            "INSERT INTO wamn_run.gate_reports (tenant_id, wiring_hash, passed, summary) VALUES ($1, $2, true, '{}'::jsonb) ON CONFLICT DO NOTHING",
            &[&request.tenant_id, &request.document.wiring_hash().as_str()],
        ).await?;
        let transaction = project.transaction().await?;
        author_workflow(&control, &transaction, request).await?;
        transaction.commit().await?;
        enable_workflow(
            system_url, scope, &mut project,
            &WorkflowVersion {
                tenant_id: request.tenant_id,
                environment: request.environment,
                application: request.application,
                workflow_id: &request.document.wiring_id,
                version: request.document.version,
            },
            request.created_by, request.manifest_digest, request.declared, events,
        ).await?;
        anyhow::Ok(())
    }.await;
    project_task.abort();
    control_task.abort();
    result
}
