//! Resolve schedule workflows against the release already loaded by the host.
use anyhow::Context as _;
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::Row as _;
use wamn_catalog::WiringDocument;
use wamn_engine::release_manifest::LoadedRelease;
use wamn_identity_protocol::{ADMIN_ROLE, workflow_user_id};
use wamn_postgres_capability::WamnPostgres;
use wamn_run_state::admission::{self, RunAdmission, RunAdmitted, RunTrigger};
use wamn_run_state::run_store::{ProductionLeaseError, ProductionLeaseErrorType};

/// What a schedule admission did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleRunAdmitted {
    /// The run is queued, newly or by an earlier admission of the same due
    /// moment.
    Queued { run_id: String },
    /// No version of the workflow with a schedule trigger is enabled. No row
    /// is written.
    NotEnabled,
    /// The enabled version does not meet the conditions of the release, or its graph
    /// does not hash to its stored hash. No row is written.
    NotServed,
    /// The key belongs to a run of another grain or input.
    Conflict,
}

/// The canonical UTC spelling of a due moment, in schedule keys and input
/// and in the schedule task's logs.
pub fn due_text(due: DateTime<Utc>) -> String {
    due.to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub(crate) fn parse_graph(json: &str) -> anyhow::Result<WiringDocument> {
    let value = serde_json::from_str(json).context("parse stored workflow graph")?;
    WiringDocument::parse(&value).context("validate stored workflow graph")
}

/// Condition the graph and freeze every resolved node digest once at admission.
pub(crate) fn node_digests(
    release: &LoadedRelease,
    application: &str,
    graph: &WiringDocument,
    hash: &str,
) -> anyhow::Result<serde_json::Value> {
    let target = release.target();
    let packages = &target
        .applications
        .get(application)
        .context("application is not served")?
        .packages;
    let actual = wamn_catalog::condition_workflow_document(
        &target.project_env_key,
        packages,
        release.manifest(),
        graph,
    )?;
    anyhow::ensure!(actual.as_str() == hash, "stored workflow hash differs");
    let components = wamn_catalog::resolve_workflow_components(
        &target.project_env_key,
        packages,
        release.manifest(),
        graph,
    )?;
    Ok(serde_json::Value::Object(
        components
            .into_iter()
            .map(|(node, component)| (node, serde_json::Value::String(component.component_digest)))
            .collect(),
    ))
}

/// Admit a due workflow under the release already loaded by the host.
pub async fn admit_schedule_run(
    postgres: &WamnPostgres,
    project: &str,
    release: &LoadedRelease,
    application: &str,
    workflow_id: &str,
    tick: DateTime<Utc>,
) -> Result<ScheduleRunAdmitted, ProductionLeaseError> {
    let target = release.target();
    if !target.applications.contains_key(application) {
        return Err(ProductionLeaseError::contract(
            "admit schedule run",
            format!("application {application:?} is not declared by the loaded release"),
        ));
    }

    let due = due_text(tick);
    let input = serde_json::json!({"due": due});
    let key = format!("schedule:{application}/{workflow_id}:{due}");
    postgres
        .executor_write(project, &target.project_env_key, async |connection| {
            let Some(row) = admission::enabled_schedule(
                connection,
                &target.project_env_key,
                &target.environment,
                application,
                workflow_id,
            )
            .await
            .map_err(|error| ProductionLeaseError::postgres("read enabled schedule", &error))?
            else {
                return Ok(ScheduleRunAdmitted::NotEnabled);
            };
            // Repeated ticks retain their original manifest and frozen nodes.
            if let Some(run_id) = admission::existing_run(
                connection,
                &target.project_env_key,
                &key,
                &input,
                RunTrigger::Schedule,
            )
            .await?
            {
                return Ok(ScheduleRunAdmitted::Queued { run_id });
            }
            let version: i32 = row.get(0);
            let graph_json: String = row.get(1);
            let hash: String = row.get(2);
            let role: Option<String> = row.get(3);
            let graph = parse_graph(&graph_json).map_err(|error| {
                ProductionLeaseError::with_source(
                    ProductionLeaseErrorType::Contract,
                    "parse stored schedule graph",
                    error,
                )
            })?;
            let Ok(nodes) = node_digests(release, application, &graph, &hash) else {
                return Ok(ScheduleRunAdmitted::NotServed);
            };
            let principal = workflow_user_id().to_string();
            let role = role.as_deref().unwrap_or(ADMIN_ROLE);
            Ok(
                match admission::admit_run(
                    connection,
                    &RunAdmission {
                        project_env_key: &target.project_env_key,
                        application,
                        manifest_digest: release.release().manifest_digest.as_str(),
                        environment: &target.environment,
                        workflow_id,
                        workflow_version: version,
                        wiring_hash: &hash,
                        execution_principal_id: &principal,
                        execution_role: role,
                        trigger: RunTrigger::Schedule,
                        invoked_by: "schedule",
                        invoked_by_user_id: None,
                        node_digests: &nodes,
                        idempotency_key: &key,
                        input: &input,
                        source_run: None,
                        root_run: None,
                        depth: None,
                    },
                )
                .await?
                {
                    RunAdmitted::Queued { run_id } => ScheduleRunAdmitted::Queued { run_id },
                    RunAdmitted::Conflict => ScheduleRunAdmitted::Conflict,
                },
            )
        })
        .await
}
