//! The virtualized `jsonata` node, admitted and run through the real queue.
//!
//! The node is a std guest: the release stage virtualizes it, and its random
//! import passes through at the version Rust std links. This case admits those
//! bytes under the tenant import policy and runs them in a one-node wiring.

use serde_json::json;
use wash_runtime::host::probes::Liveness;

use super::{
    Node, QueueScope, RouterDriver, SERVICE, TENANT, WamnJetstream, WamnPostgres, drain_one,
};
use crate::{PostgresWorkflows, StartRequest, Trigger, Workflows as _};

/// The node's declaration template, as publish renders it.
const DECLARATION: &str =
    include_str!("../../../../../../apps/platform/execution/jsonata/declaration.json.in");

/// The WMS label workflow's shape expression (docs/plan/workflow-feature.md 4.4).
const SHAPE: &str = r#"event = "insert" and new.kind = "move"
    ? [{"request_id": new.id,
        "value": {"movement_id": new.id, "pallet_id": new.pallet_id,
                  "location_id": new.to_location_id}}]
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
    pub(super) driver: &'a RouterDriver,
    pub(super) postgres: &'a WamnPostgres,
    pub(super) scope: &'a QueueScope,
    pub(super) jetstream: &'a WamnJetstream,
    pub(super) liveness: &'a Liveness,
}

pub(super) async fn run(
    admin: &tokio_postgres::Client,
    workflows: &PostgresWorkflows,
    execution: Execution<'_>,
) -> anyhow::Result<()> {
    let row = json!({"event": "insert", "new": {
        "id": "m-1", "kind": "move", "pallet_id": "p-1",
        "from_location_id": "l-0", "to_location_id": "l-2", "quantity": "4"}});
    let run = workflows
        .start(&StartRequest {
            effective_release_id: 1,
            package_id: "automation".to_owned(),
            wiring_id: "echo".to_owned(),
            wiring_version: 1,
            idempotency_key: "jsonata".to_owned(),
            input: row,
            trigger: Trigger::Automation {
                service_principal_id: SERVICE.to_owned(),
            },
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
        json!([{"request_id": "m-1", "value": {
            "movement_id": "m-1", "pallet_id": "p-1", "location_id": "l-2"}}])
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: tools/build-components all (the virtualized jsonata node)"]
async fn the_jsonata_node_shapes_a_row_through_the_queue() -> anyhow::Result<()> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../apps/target/virtualized/std-empty-environment/jsonata_expression.wasm"
    );
    let bytes = std::fs::read(path)
        .map_err(|error| anyhow::anyhow!("read {path}; run tools/build-components all: {error}"))?;
    super::run_automation(super::Mode::Jsonata(bytes)).await
}
