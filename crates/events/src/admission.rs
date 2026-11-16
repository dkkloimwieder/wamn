//! Manual admission through the authorized executor transport.
use sqlx::Row as _;
use wamn_catalog::WiringDocument;
pub use wamn_run_state::admission::RunAdmitted as ManualRunAdmitted;
use wamn_run_state::admission::{self, RunAdmission, RunTrigger};
use wamn_run_state::run_store::ProductionLeaseError;

/// One manual workflow run to admit: the enabled workflow version, the
/// release it conditioned against, who it executes as, who started it, and the
/// component digest of every node.
#[derive(Debug, Clone, Copy)]
pub struct ManualRunAdmission<'a> {
    /// The application, by the name the environment declares.
    pub application: &'a wamn_catalog::ApplicationName,
    /// The release the host serves, as its manifest digest.
    pub manifest_digest: &'a wamn_digest::Digest,
    pub environment: &'a wamn_registry::EnvName,
    pub workflow_id: &'a wamn_catalog::canonical_name::CanonicalName,
    pub workflow_version: i32,
    pub wiring_hash: &'a wamn_digest::Digest,
    pub execution_principal_id: &'a wamn_identity_protocol::PrincipalId,
    pub execution_role: &'a str,
    /// The session or PAT user that started the run.
    pub invoked_by_user_id: &'a wamn_identity_protocol::PrincipalId,
    /// `{node id: component digest}`.
    pub node_digests: &'a serde_json::Value,
    /// A repeated key with the same input admits no second run.
    pub idempotency_key: &'a str,
    pub input: &'a serde_json::Value,
}

/// Admit a manual workflow run and its queue entry in one transaction.
pub async fn admit_manual_run(
    connection: &mut sqlx::PgConnection,
    project_env_key: &wamn_registry::ProjectEnvKey,
    run: &ManualRunAdmission<'_>,
) -> Result<ManualRunAdmitted, ProductionLeaseError> {
    admission::admit_run(
        connection,
        &RunAdmission {
            project_env_key,
            application: run.application,
            manifest_digest: run.manifest_digest,
            environment: run.environment,
            workflow_id: run.workflow_id,
            workflow_version: run.workflow_version,
            wiring_hash: run.wiring_hash,
            execution_principal_id: run.execution_principal_id,
            execution_role: run.execution_role,
            trigger: RunTrigger::Manual,
            invoked_by: "manual",
            invoked_by_user_id: Some(run.invoked_by_user_id),
            node_digests: run.node_digests,
            idempotency_key: run.idempotency_key,
            input: run.input,
            source_run: None,
            root_run: None,
            depth: None,
        },
    )
    .await
}

/// The enabled version of one workflow, as the host admits a run of it.
#[derive(Debug, Clone)]
pub struct EnabledWorkflow {
    pub version: i32,
    pub graph: WiringDocument,
    pub wiring_hash: wamn_digest::Digest,
    /// `role` or `invoker`.
    pub execution_type: String,
    /// The active role of a `role` workflow.
    pub execution_role: Option<String>,
}

/// The enabled version of manual workflow `workflow_id` of `application`
/// in `environment`, or none.
pub async fn enabled_workflow(
    connection: &mut sqlx::PgConnection,
    project_env_key: &wamn_registry::ProjectEnvKey,
    environment: &wamn_registry::EnvName,
    application: &wamn_catalog::ApplicationName,
    workflow_id: &wamn_catalog::canonical_name::CanonicalName,
) -> anyhow::Result<Option<EnabledWorkflow>> {
    let Some(row) = wamn_run_state::admission::enabled_workflow(
        connection,
        project_env_key,
        environment,
        application,
        workflow_id,
    )
    .await?
    else {
        return Ok(None);
    };
    let graph_json: String = row.try_get(1)?;
    Ok(Some(EnabledWorkflow {
        version: row.try_get(0)?,
        graph: wamn_catalog::WiringDocument::parse(&serde_json::from_str(&graph_json)?)?,
        wiring_hash: row.try_get(2)?,
        execution_type: row.try_get(3)?,
        execution_role: row.try_get(4)?,
    }))
}
