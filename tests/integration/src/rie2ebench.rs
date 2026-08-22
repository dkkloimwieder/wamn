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
const FLOW_FIXTURE_ID: &str = "disp-del";
const REGISTRATION_FLOW_ID: &str = "disp-del";
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
        "schema-version": "0.1", "flow-id": FLOW_FIXTURE_ID, "version": 1,
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
        "flow-id": REGISTRATION_FLOW_ID,
        "entity": ENTITY_ID,
        "ops": ["delete"],
        "condition": null,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The registration, catalog, and legacy flow fixtures retain one coherent subscription.
    #[test]
    fn registration_and_legacy_flow_fixtures_match_their_frozen_types() {
        let reg = wamn_event_reg::EventRegistration::from_json(&registration_json())
            .expect("delete registration is a frozen EventRegistration");
        let cat = catalog().expect("catalog fixture parses");
        let flow = wamn_flow::Flow::from_json(&flow_json()).expect("legacy flow fixture parses");

        assert_eq!(
            reg.catalog_id, cat.catalog_id,
            "the registration must target the retained catalog fixture"
        );
        assert_eq!(
            reg.flow_id, flow.flow_id,
            "the registration must target the retained flow fixture"
        );
        assert_eq!(
            reg.ops.as_slice(),
            &[wamn_event_wire::Op::Delete],
            "the historical registration must carry exactly one delete operation"
        );

        flow.validate(&Default::default())
            .expect("legacy flow fixture validates");
        let entry_nodes = flow
            .nodes
            .iter()
            .filter(|node| node.entry_kind().is_some())
            .collect::<Vec<_>>();
        assert_eq!(
            entry_nodes.len(),
            1,
            "the historical flow fixture must carry exactly one entry node"
        );
        assert_eq!(
            entry_nodes[0].entry_kind(),
            Some(wamn_flow::EntryKind::Event),
            "the historical flow fixture's sole entry must remain event-shaped"
        );
    }

    /// The registration entity resolves to the catalog's sole historical table mapping.
    #[test]
    fn catalog_fixture_contains_the_historical_target_mapping() {
        let reg = wamn_event_reg::EventRegistration::from_json(&registration_json())
            .expect("delete registration is a frozen EventRegistration");
        let cat = catalog().expect("catalog fixture parses");
        assert_eq!(cat.catalog_id, CATALOG_ID);
        let [entity] = cat.entities.as_slice() else {
            panic!("the historical catalog fixture must carry exactly one entity")
        };
        assert_eq!(
            reg.entity, entity.id,
            "the registration entity must resolve to the catalog's sole entity"
        );
        assert_eq!(entity.id, ENTITY_ID);
        assert_eq!(entity.name, TABLE);
    }
}
