//! Frozen fixture guards remain from the retired reader-inclusive replica-identity harness.
//!
//! The former `rie2ebench` command body was retired after its command route and
//! Job had been removed. These tests prove only that the historical event
//! registration, legacy flow graph, and catalog fixtures retain their frozen
//! shapes. They do not execute PostgreSQL, the CDC reader, JetStream, the
//! materializer, or a replica-identity cutover.

/// Historical entity-to-table mapping used by the retired harness.
const ENTITY_ID: &str = "evt_disp";
const TABLE: &str = "dispositions";
const CATALOG_ID: &str = "ricat";
const FLOW_ID: &str = "disp-del";
const REG_ID: &str = "r-del";

const CATALOG_JSON: &str = r#"{
  "schema-version": "0.1",
  "catalog-id": "ricat",
  "version": 1,
  "entities": [
    { "id": "evt_disp", "name": "dispositions", "fields": [
      { "id": "site", "name": "site", "type": { "kind": "text" } }
    ] }
  ]
}"#;

fn catalog() -> anyhow::Result<wamn_schema_model::Catalog> {
    wamn_schema_model::Catalog::from_json(CATALOG_JSON)
        .map_err(|error| anyhow::anyhow!("rie2ebench fixture catalog parse: {error}"))
}

/// Frozen legacy flow-graph fixture retained as a schema drift guard.
fn flow_json() -> String {
    serde_json::json!({
        "schema-version": "0.1", "flow-id": FLOW_ID, "version": 1,
        "nodes": [{"id": "event", "type": "event"}],
    })
    .to_string()
}

/// Frozen delete-only event-registration fixture retained as a schema drift guard.
fn registration_json() -> String {
    serde_json::json!({
        "schema-version": "0.1",
        "registration-id": REG_ID,
        "catalog-id": CATALOG_ID,
        "flow-id": FLOW_ID,
        "entity": ENTITY_ID,
        "ops": ["delete"],
        "condition": null,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The registration and legacy flow-graph fixtures retain their frozen types.
    #[test]
    fn registration_and_legacy_flow_fixtures_match_their_frozen_types() {
        let reg = wamn_event_reg::EventRegistration::from_json(&registration_json())
            .expect("delete registration is a frozen EventRegistration");
        assert!(
            reg.ops
                .iter()
                .any(|op| format!("{op:?}").to_lowercase().contains("delete")),
            "the historical registration fixture must remain delete-subscribed"
        );

        let flow = wamn_flow::Flow::from_json(&flow_json()).expect("legacy flow fixture parses");
        flow.validate(&Default::default())
            .expect("legacy flow fixture validates");
        assert_eq!(
            flow.entry_node().map(|node| node.node_type.as_str()),
            Some("event")
        );
    }

    /// The catalog fixture retains the historical entity-to-table mapping.
    #[test]
    fn catalog_fixture_contains_the_historical_target_mapping() {
        let cat = catalog().expect("catalog fixture parses");
        assert_eq!(cat.catalog_id, CATALOG_ID);
        assert!(
            cat.entities
                .iter()
                .any(|entity| entity.id == ENTITY_ID && entity.name == TABLE),
            "catalog fixture must carry evt_disp -> dispositions"
        );
    }
}
