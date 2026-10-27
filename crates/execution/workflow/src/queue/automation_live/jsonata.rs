//! The virtualized `jsonata` node, admitted and run through the real queue.
//!
//! The node is a std guest: the release stage virtualizes it, and its random
//! import passes through at the version Rust std links. This case admits those
//! bytes under the tenant import policy and runs them in a one-node wiring.

use serde_json::json;
use wash_runtime::host::probes::Liveness;

use wamn_runtime::plugins::wamn_postgres::EventBatchAdmission;

use super::{Node, QueueScope, RouterDriver, TENANT, WamnJetstream, WamnPostgres, drain_one};
use crate::{WorkflowErrorType, Workflows as _};

/// The node's declaration template, as publish renders it.
const DECLARATION: &str =
    include_str!("../../../../../../apps/platform/execution/jsonata/declaration.json.in");

/// The WMS label workflow's shape expression (docs/plan/workflow-feature.md 4.4).
const SHAPE: &str = r#"event = "insert" and new.kind = "move"
    ? [{"request_id": new.id,
        "value": {"label_key": new.idempotency_key, "movement_id": new.id,
                  "packaging_id": new.packaging_id, "location_id": new.to_location_id}}]
    : []"#;

pub(super) fn node(bytes: Vec<u8>) -> Node {
    let declaration = DECLARATION
        .replace("__TENANT_ID__", TENANT)
        .replace("__PACKAGE_ID__", "automation")
        .replace("__PACKAGE_VERSION__", "1.0.0");
    Node {
        bytes,
        declaration: serde_json::from_str(&declaration).expect("the declaration template is JSON"),
        operation: "wamn:node/handler@0.1.0".to_owned(),
        params: json!({"expression": SHAPE}),
        serving: json!({"statements": {}}),
    }
}

pub(super) struct Execution<'a> {
    pub(super) release: &'a wamn_engine::release_manifest::LoadedRelease,
    pub(super) driver: &'a RouterDriver,
    pub(super) postgres: &'a WamnPostgres,
    pub(super) scope: &'a QueueScope,
    pub(super) jetstream: &'a WamnJetstream,
    pub(super) liveness: &'a Liveness,
}

pub(super) async fn run(
    admin: &tokio_postgres::Client,
    workflows: &super::WorkflowsFixture,
    execution: Execution<'_>,
) -> anyhow::Result<()> {
    let row = json!({"event": "insert", "new": {
        "id": "m-1", "idempotency_key": "k-1", "kind": "move", "packaging_id": "p-1",
        "from_location_id": "l-0", "to_location_id": "l-2", "quantity": "4"}});
    let run = workflows
        .start(&super::StartInput {
            package_id: "automation".to_owned(),
            wiring_id: "echo-manual".to_owned(),
            idempotency_key: "jsonata".to_owned(),
            input: row,
        })
        .await?;
    assert!(
        drain_one(
            execution.driver,
            execution.postgres,
            execution.jetstream,
            execution.scope,
            30_000,
            execution.liveness,
        )
        .await?,
        "the queue claims the started run"
    );
    let row = admin
        .query_one(
            "SELECT status, result_json::text FROM wamn_run.runs WHERE run_id = $1",
            &[&run],
        )
        .await?;
    assert_eq!(row.get::<_, String>(0), "completed");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&row.get::<_, String>(1))?,
        json!([{"request_id": "m-1", "value": {"label_key": "k-1",
            "movement_id": "m-1", "packaging_id": "p-1", "location_id": "l-2"}}])
    );
    event_run(admin, workflows, &execution).await
}

/// An event-started run enters the queue through the host's executor
/// credential, under the release's authority (wamn-upl3.6). The queue runs it
/// as the workflow user, and the contract lists, parks and releases it.
async fn event_run(
    admin: &tokio_postgres::Client,
    workflows: &super::WorkflowsFixture,
    execution: &Execution<'_>,
) -> anyhow::Result<()> {
    let envelope = json!({"op": "insert", "new": {
        "id": "m-2", "idempotency_key": "k-2", "kind": "move", "packaging_id": "p-2",
        "from_location_id": "l-0", "to_location_id": "l-3", "quantity": "1"},
        "package_id": "automation", "entity": "movement", "table": "movement", "lsn": 7,
        "txid": 1, "commit_ts": "2026-10-25T00:00:00Z"})
    .to_string();
    let source_id = wamn_event_wire::msg_id(&execution.scope.project, "test", 7);
    let mut admission = EventBatchAdmission {
        source_package: "automation",
        entity: "movement",
        envelope: &envelope,
        sequence: 7,
        source_event_id: &source_id,
    };
    execution
        .postgres
        .admit_event_batch(&execution.scope.project, execution.release, &admission)
        .await?;
    execution
        .postgres
        .admit_event_batch(&execution.scope.project, execution.release, &admission)
        .await?;
    let runs = admin
        .query(
            "SELECT run_id FROM wamn_run.runs WHERE trigger_source = 'event'",
            &[],
        )
        .await?;
    assert_eq!(runs.len(), 1, "redelivery admits no second run");
    let run: String = runs[0].get(0);
    let mut other: serde_json::Value = serde_json::from_str(&envelope)?;
    other["new"]["id"] = "m-3".into();
    let other = other.to_string();
    admission.envelope = &other;
    assert!(
        execution
            .postgres
            .admit_event_batch(&execution.scope.project, execution.release, &admission)
            .await
            .is_err(),
        "changed input must conflict"
    );

    let listed = workflows.list(10).await?;
    let entry = listed
        .iter()
        .find(|entry| entry.run_id == run)
        .expect("list shows the event run");
    assert_eq!(entry.trigger_source.as_deref(), Some("event"));
    assert!(entry.queued && !entry.parked);
    workflows.park(&run).await?;
    assert!(
        !drain_one(
            execution.driver,
            execution.postgres,
            execution.jetstream,
            execution.scope,
            30_000,
            execution.liveness,
        )
        .await?,
        "no claim takes a parked event run"
    );
    workflows.release(&run).await?;
    assert!(
        drain_one(
            execution.driver,
            execution.postgres,
            execution.jetstream,
            execution.scope,
            30_000,
            execution.liveness,
        )
        .await?,
        "the queue claims the released event run"
    );
    let stored = admin
        .query_one(
            "SELECT status, result_json::text, trigger_source, execution_principal_id::text, \
                    service_principal_id IS NULL FROM wamn_run.runs WHERE run_id = $1",
            &[&run],
        )
        .await?;
    assert_eq!(stored.get::<_, String>(0), "completed");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&stored.get::<_, String>(1))?,
        json!([{"request_id": "m-2", "value": {"label_key": "k-2",
            "movement_id": "m-2", "packaging_id": "p-2", "location_id": "l-3"}}])
    );
    assert_eq!(stored.get::<_, String>(2), "event");
    assert_eq!(
        stored.get::<_, String>(3),
        wamn_project_state::workflow_user_id().to_string()
    );
    assert!(
        stored.get::<_, bool>(4),
        "an event run has no service principal"
    );
    assert_eq!(
        workflows.park(&run).await.unwrap_err().error_type(),
        WorkflowErrorType::NotParkable
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: wamn build apps/wamn_wms (the virtualized jsonata node)"]
async fn the_jsonata_node_shapes_a_row_through_the_queue() -> anyhow::Result<()> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../apps/target/wamn/wamn_wms/components/jsonata.wasm"
    );
    wamn_schema_generator::receipt::check_current(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/wamn_wms"),
    )?;
    let bytes = std::fs::read(path)
        .map_err(|error| anyhow::anyhow!("read {path}; run wamn build apps/wamn_wms: {error}"))?;
    super::run_automation(super::Mode::Jsonata(bytes)).await
}
