// SPDX-License-Identifier: Apache-2.0

use std::path::Path;

const FLOW_MODEL_SOURCE: &str =
    include_str!("../../../crates/execution/flow-model/src/node_contract.rs");
const STANDARD_NODES_SOURCE: &str =
    include_str!("../../../crates/execution/standard-nodes/src/lib.rs");
const POSTGRES_SOURCE: &str =
    include_str!("../../../crates/execution/standard-nodes/src/postgres.rs");
const POLICY_SOURCE: &str = include_str!("../../../crates/execution/standard-nodes/src/policy.rs");
const STANDARD_NODES_MANIFEST: &str =
    include_str!("../../../crates/execution/standard-nodes/Cargo.toml");
const FLOWRUNNER_SOURCE: &str = include_str!("../../../components/execution/flowrunner/src/lib.rs");
const INTEGRATION_SOURCE: &str = include_str!("../../integration/src/lib.rs");
const ORCHESTRATOR_SOURCE: &str = include_str!("../../orchestrator/src/main.rs");
const STATE_OWNERS: &str = include_str!("../../../architecture/state-owners.json");

fn repository_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
}

#[test]
fn postgres_entity_and_its_catalog_carrier_stay_retired() {
    assert!(!FLOW_MODEL_SOURCE.contains("fn catalog_json("));
    assert!(!POLICY_SOURCE.contains("fn catalog_json("));
    assert!(!FLOWRUNNER_SOURCE.contains("fn catalog_json("));

    for retired in [
        "struct PostgresEntity",
        "fn api_to_pg(",
        "wamn_schema_model",
        "\"postgres\",",
    ] {
        assert!(
            !POSTGRES_SOURCE.contains(retired) && !STANDARD_NODES_SOURCE.contains(retired),
            "retired entity-node surface {retired:?} returned",
        );
    }
    assert!(!STANDARD_NODES_MANIFEST.contains("wamn-schema-model"));
}

#[test]
fn raw_sql_and_response_row_shaping_survive_entity_retirement() {
    for survivor in ["struct PostgresQuery", "fn pg_to_api(", "shape_rows"] {
        assert!(
            POSTGRES_SOURCE.contains(survivor),
            "retirement removed retained Postgres surface {survivor:?}",
        );
    }
    for dependency in ["wamn-entity-access", "wamn-pg-core"] {
        assert!(STANDARD_NODES_MANIFEST.contains(dependency));
    }
    assert!(STANDARD_NODES_SOURCE.contains("\"postgres-query\","));
    assert!(STATE_OWNERS.contains("standard-nodes-raw-sql"));
    assert!(STATE_OWNERS.contains("\"flowrunner\""));
    assert!(!STATE_OWNERS.contains("standard-nodes-entity"));
}

#[test]
fn entity_impactproof_surface_stays_deleted() {
    let root = repository_root();
    for retired in [
        "tests/integration/src/impactproof.rs",
        "deploy/gates/impactproof-job.yaml",
    ] {
        assert!(
            !root.join(retired).exists(),
            "retired file {retired} returned"
        );
    }
    assert!(!INTEGRATION_SOURCE.contains("pub mod impactproof;"));
    assert!(!ORCHESTRATOR_SOURCE.contains("Impactproof"));
    assert!(!ORCHESTRATOR_SOURCE.contains("impactproof::"));
}
