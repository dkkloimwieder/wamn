//! Repository-level conformance for the canonical flow graph contract.

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use wamn_flow::{Flow, ResolvedInterfaces};

    const REQUEST_FLOW: &str = r#"{
      "schema-version": "0.1",
      "flow-id": "conformance-request",
      "version": 1,
      "nodes": [
        {
          "id": "request",
          "type": "request",
          "config": {
            "input-schema": {
              "type": "object",
              "properties": {"message": {"type": "string"}},
              "required": ["message"],
              "additionalProperties": false
            }
          }
        },
        {"id": "shape", "type": "transform"},
        {"id": "response", "type": "respond", "config": {"status": 200}}
      ],
      "edges": [
        {"from": "request", "to": "shape"},
        {"from": "shape", "to": "response"}
      ]
    }"#;

    fn interfaces() -> ResolvedInterfaces {
        BTreeMap::from([("transform".to_string(), vec!["main".to_string()])])
    }

    #[test]
    fn typed_entry_and_resolved_ports_validate() {
        let flow = Flow::from_json(REQUEST_FLOW).expect("typed request flow parses");

        flow.validate(&interfaces())
            .expect("typed request flow validates");
        assert_eq!(
            flow.entry_node().expect("one typed entry").node_type,
            "request"
        );
    }

    #[test]
    fn legacy_trigger_and_scalar_entry_are_rejected() {
        let legacy = r#"{
          "schema-version": "0.1",
          "flow-id": "legacy",
          "version": 1,
          "trigger": {"type": "manual"},
          "entry": "shape",
          "nodes": [{"id": "shape", "type": "transform"}]
        }"#;

        assert!(Flow::from_json(legacy).is_err());
    }

    #[test]
    fn canonical_identity_ignores_source_json_formatting() {
        let compact = REQUEST_FLOW.split_whitespace().collect::<String>();
        let pretty = Flow::from_json(REQUEST_FLOW).expect("pretty flow parses");
        let compact = Flow::from_json(&compact).expect("compact flow parses");

        assert_eq!(pretty.canonical_bytes(), compact.canonical_bytes());
        assert_eq!(pretty.graph_hash(), compact.graph_hash());
    }
}
