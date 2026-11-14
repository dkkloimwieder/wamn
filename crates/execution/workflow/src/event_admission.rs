//! Resolve matching event workflows and admit their runs as one batch.
use crate::admission::{node_digests, parse_graph};
use crate::events::EventTrigger;
use sqlx::Row as _;
use wamn_engine::release_manifest::LoadedRelease;
use wamn_event_wire::SourceEvent;
use wamn_identity_protocol::{ADMIN_ROLE, workflow_user_id};
use wamn_run_state::admission::{self, RunAdmission, RunAdmitted, RunTrigger};
use wamn_run_state::run_store::ProductionLeaseError;
use wamn_runtime::plugins::wamn_jetstream::EventBatchAdmission;
use wamn_runtime::plugins::wamn_postgres::WamnPostgres;

/// Re-read trigger rows, resolve their graphs and actor, and insert all runs
/// and queue entries before committing. Any refusal rolls back the batch.
pub async fn admit_event_batch(
    postgres: &WamnPostgres,
    project: &str,
    release: &LoadedRelease,
    admission: &EventBatchAdmission<'_>,
) -> Result<(), ProductionLeaseError> {
    let target = release.target();
    let project_env_key = &target.project_env_key;
    postgres
        .executor_write(project, project_env_key, async |connection| {
            let source = SourceEvent::decode(admission.envelope.as_bytes())
                .map_err(|error| ProductionLeaseError::invalid("decode event", error))?;
            if admission.sequence == 0
                || source
                    .identity(
                        project_env_key,
                        project,
                        &target.environment,
                        &[admission.source_event_id],
                    )
                    .is_none()
            {
                return Err(ProductionLeaseError::contract(
                    "identify event",
                    "invalid event sequence or source identity",
                ));
            }
            // Different packages can share an entity subject. Only its own
            // consumer admits the event; the others settle it without runs.
            if source.source() != (admission.source_package, admission.entity) {
                return Ok(());
            }
            let names: Vec<_> = target.applications.keys().cloned().collect();
            let packages = target
                .applications
                .values()
                .flat_map(|application| application.packages.iter().cloned())
                .collect();
            let rows = admission::event_workflows(
                connection,
                project_env_key,
                &target.environment,
                Some(&names),
            )
            .await
            .map_err(|error| ProductionLeaseError::storage("read event workflows", error))?;
            let mut matches = Vec::new();
            for row in rows {
                let event = decode_event_trigger(&row)?;
                if let Some(input) = event
                    .matches(&source, admission.sequence, &packages, project_env_key)
                    .map_err(|error| {
                        ProductionLeaseError::invalid("filter event workflow", error)
                    })?
                {
                    matches.push((row, event, input));
                }
            }
            if matches.is_empty() {
                return Ok(());
            }
            let actor = match &source {
                SourceEvent::Cdc(envelope) => {
                    admission::history_actor(connection, envelope).await?
                }
                SourceEvent::Derived(_) => None,
            };
            let parent = match &source {
                SourceEvent::Cdc(envelope) => envelope.causation.as_ref(),
                SourceEvent::Derived(event) => Some(&event.causation),
            };
            let source_run = parent.map(|parent| parent.run.as_str());
            let root_run = parent.map(|parent| parent.root.as_str());
            let depth = parent
                .map(|parent| i32::try_from(parent.depth + 1))
                .transpose()
                .map_err(|error| ProductionLeaseError::invalid("decode event depth", error))?;
            let principal = workflow_user_id().to_string();
            let delivery = format!("event:{}", admission.source_event_id);
            for (row, event, input) in matches {
                let key = serde_json::json!([
                    "event",
                    target.environment,
                    event.application,
                    event.workflow_id,
                    delivery
                ])
                .to_string();
                // A repeat keeps the release and nodes from its original admission.
                if admission::existing_run(
                    connection,
                    project_env_key,
                    &key,
                    &input,
                    RunTrigger::Event,
                )
                .await?
                .is_some()
                {
                    continue;
                }
                let version: i32 = row.get(7);
                let graph_json: String = row.get(8);
                let hash: String = row.get(9);
                let role = row
                    .get::<Option<String>, _>(10)
                    .unwrap_or_else(|| ADMIN_ROLE.to_owned());
                let graph = parse_graph(&graph_json).map_err(|error| {
                    ProductionLeaseError::invalid("parse event workflow", error)
                })?;
                let digests =
                    node_digests(release, &event.application, &graph, &hash).map_err(|error| {
                        ProductionLeaseError::invalid("resolve event workflow", error)
                    })?;
                let digest = release.release().manifest_digest.as_str();
                if admission::admit_run(
                    connection,
                    &RunAdmission {
                        project_env_key,
                        application: &event.application,
                        manifest_digest: digest,
                        environment: &target.environment,
                        workflow_id: &event.workflow_id,
                        workflow_version: version,
                        wiring_hash: &hash,
                        execution_principal_id: &principal,
                        execution_role: &role,
                        trigger: RunTrigger::Event,
                        invoked_by: &delivery,
                        invoked_by_user_id: actor.as_deref(),
                        node_digests: &digests,
                        idempotency_key: &key,
                        input: &input,
                        source_run,
                        root_run,
                        depth,
                    },
                )
                .await?
                    == RunAdmitted::Conflict
                {
                    return Err(ProductionLeaseError::contract(
                        "admit event run",
                        "event idempotency conflict",
                    ));
                }
            }
            Ok(())
        })
        .await
}

/// Pull and settle application deliveries through the broker transport.
pub async fn admit_application_events(
    jetstream: &wamn_runtime::plugins::wamn_jetstream::WamnJetstream,
    postgres: &WamnPostgres,
    project: &str,
    release: &LoadedRelease,
) -> anyhow::Result<()> {
    let target = release.target();
    let triggers = postgres
        .executor_read(project, &target.project_env_key, async |connection| {
            admission::event_workflows(
                connection,
                &target.project_env_key,
                &target.environment,
                None,
            )
            .await?
            .iter()
            .map(|row| decode_event_trigger(row).map_err(anyhow::Error::from))
            .collect::<anyhow::Result<Vec<_>>>()
        })
        .await?;
    let sources = triggers
        .iter()
        .filter(|trigger| target.applications.contains_key(&trigger.application))
        .map(|trigger| {
            (
                trigger.application.as_str(),
                trigger.source_package.as_str(),
                trigger.entity.as_str(),
            )
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    jetstream
        .admit_application_events(postgres, project, release, &sources, admit_event_batch)
        .await
}

/// Decode stored event trigger facts for workflow orchestration.
fn decode_event_trigger(row: &sqlx::postgres::PgRow) -> Result<EventTrigger, ProductionLeaseError> {
    let ops: String = row.get(4);
    let sequence: i64 = row.get(6);
    Ok(EventTrigger {
        application: row.get(0),
        workflow_id: row.get(1),
        source_package: row.get(2),
        entity: row.get(3),
        ops: serde_json::from_str(&ops)
            .map_err(|error| ProductionLeaseError::invalid("decode event operations", error))?,
        condition: row.get(5),
        enable_sequence: u64::try_from(sequence)
            .map_err(|error| ProductionLeaseError::invalid("decode enable sequence", error))?,
    })
}
